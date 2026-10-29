//! Reading a webcam (webcams.WebcamReader): opening it with the Choice picked, handing out its frames as packets (an
//! MJPEG camera's JPEGs as they came, raw frames for the source's own decoder), and saying why it gives nothing: gone,
//! held by another program, or opened but silent. Linux reads V4L2 directly with memory-mapped buffers.

use crate::webcams::{Choice, Device};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::i18n::{n_, tr};
use telescope_core::media::{Decoded, Decoder, FeedEnd, Packet};

pub const GONE_TEXT: &str = n_("The webcam isn't plugged in. Plug it back in and it carries on.");
pub const BUSY_TEXT: &str = n_("Another app is using the webcam. Close it there and it starts here.");
pub const NO_FRAMES_TEXT: &str = n_("The webcam isn't sending anything. Try another resolution or frame rate.");

/// How an opening went, for the plugin: ("ok", the Choice's text), ("busy", the program holding it or ""),
/// ("gone", "") or ("silent", "").
pub type Report = std::sync::Arc<dyn Fn(&str, &str) + Send + Sync>;

/// A camera opened for reading: frames come out of read() until it fails.
pub trait Capture: Send {
    /// The next frame's bytes as the camera sent them.
    fn read(&mut self) -> Result<Vec<u8>, String>;
    /// Unblocks a read from another thread.
    fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper>;
    /// What the driver actually gave when it adjusted the request, which raw frames have to be decoded as.
    fn negotiated(&self) -> Option<Choice> {
        None
    }
}

/// Opens a capture of device asking for choice: Err((opened at all, why)).
pub type Opener = std::sync::Arc<dyn Fn(&Device, &Choice) -> Result<Box<dyn Capture>, (bool, String)> + Send + Sync>;

/// What a reader needs, so tests can stand in for cameras.
#[derive(Clone)]
pub struct ReaderDeps {
    /// The camera as it's plugged in now, or None.
    pub locate: std::sync::Arc<dyn Fn() -> Option<Device> + Send + Sync>,
    /// What to ask it for.
    pub want: std::sync::Arc<dyn Fn(&Device) -> Option<Choice> + Send + Sync>,
    pub report: Report,
    pub opener: Opener,
    /// The program holding a node ("" if none or unknown).
    pub users: std::sync::Arc<dyn Fn(&str) -> String + Send + Sync>,
    /// Whether a node is still there (Linux checks the /dev node).
    pub exists: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

/// A feed of one webcam: the first frame the opening checked comes out first.
pub struct WebcamFeed {
    capture: Box<dyn Capture>,
    first: Option<Vec<u8>>,
    deps: ReaderDeps,
    location: String,
    waiting: std::sync::Arc<std::sync::Mutex<String>>,
}

/// Opens the camera and reads one frame to check it sends; on failure says why (and why in waiting_text).
pub fn open(deps: &ReaderDeps, waiting: &std::sync::Arc<std::sync::Mutex<String>>) -> Result<(Choice, WebcamFeed), String> {
    let say = |text: &str, state: &str, detail: &str| {
        *waiting.lock().unwrap() = tr(text);
        (deps.report)(state, detail);
        Err(tr(text))
    };
    let Some(device) = (deps.locate)() else { return say(GONE_TEXT, "gone", "") };
    let Some(choice) = (deps.want)(&device) else { return say(NO_FRAMES_TEXT, "silent", "") };
    let opened = (deps.opener)(&device, &choice).map_err(|(opened, why)| (opened, why)).and_then(|mut cap| {
        cap.read().map(|first| (cap, first)).map_err(|why| (true, why))
    });
    match opened {
        Ok((capture, first)) => {
            (deps.report)("ok", &choice.describe());
            let choice = capture.negotiated().unwrap_or(choice);
            let feed = WebcamFeed { capture, first: Some(first), deps: deps.clone(), location: device.location.clone(), waiting: waiting.clone() };
            Ok((choice, feed))
        }
        Err((opened, why)) => {
            log::info!("{} gave nothing: {why}", device.name);
            // Gone, held by another program, or opened but silent
            if (deps.locate)().is_none() || !(deps.exists)(&device.location) {
                return say(GONE_TEXT, "gone", "");
            }
            let user = (deps.users)(&device.location);
            if !user.is_empty() || !opened {
                return say(BUSY_TEXT, "busy", &user);
            }
            say(NO_FRAMES_TEXT, "silent", "")
        }
    }
}

impl telescope_core::media::VideoFeed for WebcamFeed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        let data = match self.first.take() {
            Some(first) => first,
            None => match self.capture.read() {
                Ok(d) => d,
                Err(why) => {
                    let gone = (self.deps.locate)().is_none() || !(self.deps.exists)(&self.location);
                    let (text, state) = if gone { (GONE_TEXT, "gone") } else { (NO_FRAMES_TEXT, "silent") };
                    *self.waiting.lock().unwrap() = tr(text);
                    (self.deps.report)(state, "");
                    return Err(FeedEnd::Error(why));
                }
            },
        };
        Ok(Packet { data, time_ns: telescope_core::time::monotonic_ns() })
    }

    fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
        self.capture.stopper()
    }
}

/// What crossed the cable for one packet: the JPEG, or about two bytes a pixel raw (YUYV).
pub fn cable_bytes(choice: &Choice, packet_len: usize) -> u64 {
    if choice.compressed() { packet_len as u64 } else { choice.width as u64 * choice.height as u64 * 2 }
}

/// Decodes a webcam's raw frames to I420 (what OpenCV did for the Python app).
pub struct RawDecoder {
    pub fourcc: String,
    pub width: u32,
    pub height: u32,
}

impl Decoder for RawDecoder {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        match raw_to_i420(&self.fourcc, self.width, self.height, &packet.data) {
            Some(frame) => Decoded { newest: Some(frame), count: 1 },
            None => Decoded::default(),
        }
    }
    fn hardware(&self) -> bool {
        false
    }
}

/// A raw frame as I420; None if it's too short or a format we don't know.
pub fn raw_to_i420(fourcc: &str, width: u32, height: u32, data: &[u8]) -> Option<Frame> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![128u8; w * h + 2 * cw * ch];
    let (y, uv) = out.split_at_mut(w * h);
    let (u, v) = uv.split_at_mut(cw * ch);
    match fourcc {
        "YUYV" | "UYVY" => {
            if data.len() < w * h * 2 {
                return None;
            }
            let (yo, uo, vo) = if fourcc == "YUYV" { (0, 1, 3) } else { (1, 0, 2) };
            for row in 0..h {
                for pair in 0..w / 2 {
                    let i = (row * w + pair * 2) * 2;
                    y[row * w + pair * 2] = data[i + yo];
                    y[row * w + pair * 2 + 1] = data[i + yo + 2];
                    if row % 2 == 0 {
                        u[(row / 2) * cw + pair] = data[i + uo];
                        v[(row / 2) * cw + pair] = data[i + vo];
                    }
                }
            }
        }
        "NV12" | "YU12" | "YV12" => {
            if data.len() < w * h + 2 * cw * ch {
                return None;
            }
            y.copy_from_slice(&data[..w * h]);
            let rest = &data[w * h..];
            match fourcc {
                "NV12" => {
                    for i in 0..cw * ch {
                        u[i] = rest[2 * i];
                        v[i] = rest[2 * i + 1];
                    }
                }
                "YU12" => {
                    u.copy_from_slice(&rest[..cw * ch]);
                    v.copy_from_slice(&rest[cw * ch..2 * cw * ch]);
                }
                _ => {
                    v.copy_from_slice(&rest[..cw * ch]);
                    u.copy_from_slice(&rest[cw * ch..2 * cw * ch]);
                }
            }
        }
        "GREY" => {
            if data.len() < w * h {
                return None;
            }
            y.copy_from_slice(&data[..w * h]);
        }
        "RGB3" | "BGR3" => {
            if data.len() < w * h * 3 {
                return None;
            }
            let rgb: Vec<u8> = if fourcc == "BGR3" { data[..w * h * 3].chunks_exact(3).flat_map(|p| [p[2], p[1], p[0]]).collect() } else { data[..w * h * 3].to_vec() };
            return Some(crate::convert::rgb_to_i420(&rgb, width, height, 3, false));
        }
        _ => return None,
    }
    Some(Frame::new(width, height, PixelFormat::I420, out))
}

/// Rows stride bytes apart (a driver may pad them) packed tight for raw_to_i420, in V4L2's single-buffer plane layouts.
pub fn tight_rows(fourcc: &str, width: u32, height: u32, stride: u32, data: &[u8]) -> Vec<u8> {
    let (w, h, stride) = (width as usize, height as usize, stride as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    // (row bytes, rows, stride) of each plane in turn
    let planes: Vec<(usize, usize, usize)> = match fourcc {
        "YUYV" | "UYVY" => vec![(w * 2, h, stride)],
        "RGB3" | "BGR3" => vec![(w * 3, h, stride)],
        "GREY" => vec![(w, h, stride)],
        "NV12" => vec![(w, h, stride), (cw * 2, ch, stride)],
        "YU12" | "YV12" => vec![(w, h, stride), (cw, ch, stride / 2), (cw, ch, stride / 2)],
        _ => return data.to_vec(),
    };
    if planes.iter().all(|&(row, _, step)| step <= row) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(planes.iter().map(|&(row, rows, _)| row * rows).sum());
    let mut at = 0;
    for (row, rows, step) in planes {
        for r in 0..rows {
            let start = at + r * step;
            let Some(line) = data.get(start..start + row) else { return out };
            out.extend_from_slice(line);
        }
        at += rows * step;
    }
    out
}

/// A webcam's JPEG is decoded by media's MJPEG decoder; raw frames by RawDecoder.
pub fn is_jpeg(choice: &Choice) -> bool {
    choice.compressed()
}

// ── Linux: V4L2 with memory-mapped buffers ──────────────────────────────────

/// An ioctl on an open file.
#[cfg(target_os = "linux")]
pub struct Fd<'a>(pub &'a std::fs::File);

#[cfg(target_os = "linux")]
impl crate::webcams::Ioctl for Fd<'_> {
    fn call(&self, request: u32, buf: &mut [u8]) -> bool {
        use std::os::fd::AsRawFd;
        // SAFETY: buf is as big as the request says (the request encodes its size)
        unsafe { libc::ioctl(self.0.as_raw_fd(), request as libc::c_ulong, buf.as_mut_ptr()) >= 0 }
    }
}

#[cfg(target_os = "linux")]
pub mod v4l2 {
    use super::Capture;
    use crate::webcams::{code, put_u32, u32_at, Choice, Ioctl};
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const TYPE_CAPTURE: u32 = 1;
    const MEMORY_MMAP: u32 = 1;
    const BUFFERS: u32 = 4;
    const BUF_SIZE: usize = 88;
    const VIDIOC_S_FMT: u32 = 0xc0d0_5605;
    const VIDIOC_REQBUFS: u32 = 0xc014_5608;
    const VIDIOC_QUERYBUF: u32 = 0xc058_5609;
    const VIDIOC_QBUF: u32 = 0xc058_560f;
    const VIDIOC_DQBUF: u32 = 0xc058_5611;
    const VIDIOC_STREAMON: u32 = 0x4004_5612;
    const VIDIOC_STREAMOFF: u32 = 0x4004_5613;
    const VIDIOC_S_PARM: u32 = 0xc0cc_5616;
    /// How long a read waits for a frame before the camera counts as silent.
    const READ_TIMEOUT_MS: i32 = 5000;

    pub struct V4l2Capture {
        file: File,
        format: Choice,
        stride: u32,
        maps: Vec<(*mut libc::c_void, usize)>,
        stop: Arc<AtomicBool>,
    }

    // SAFETY: the mappings belong to this capture alone and are only touched from the thread reading it
    unsafe impl Send for V4l2Capture {}

    struct Stopper(Arc<AtomicBool>);

    impl telescope_core::media::FeedStopper for Stopper {
        fn stop(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn buffer(index: u32) -> [u8; BUF_SIZE] {
        let mut b = [0u8; BUF_SIZE];
        put_u32(&mut b, 0, index);
        put_u32(&mut b, 4, TYPE_CAPTURE);
        put_u32(&mut b, 60, MEMORY_MMAP);
        b
    }

    /// Opens path asking for choice: Err((opened at all, why)).
    pub fn open(path: &str, choice: &Choice) -> Result<Box<dyn Capture>, (bool, String)> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(path)
            .map_err(|e| (false, e.to_string()))?;
        let io = super::Fd(&file);
        let mut fmt = [0u8; 208];
        put_u32(&mut fmt, 0, TYPE_CAPTURE);
        put_u32(&mut fmt, 8, choice.width);
        put_u32(&mut fmt, 12, choice.height);
        if choice.fourcc != "RAW" {
            put_u32(&mut fmt, 16, code(&choice.fourcc));
        }
        if !io.call(VIDIOC_S_FMT, &mut fmt) {
            return Err((false, "the camera refused the format (busy?)".into()));
        }
        // The driver writes back what it can do, which may not be what was asked
        let format = Choice { fourcc: String::from_utf8_lossy(&fmt[16..20]).into_owned(), width: u32_at(&fmt, 8), height: u32_at(&fmt, 12), fps: choice.fps };
        let stride = u32_at(&fmt, 24);
        if choice.fps > 0.0 {
            let mut parm = [0u8; 204];
            put_u32(&mut parm, 0, TYPE_CAPTURE);
            put_u32(&mut parm, 12, 1000);
            put_u32(&mut parm, 16, (choice.fps * 1000.0).round() as u32);
            io.call(VIDIOC_S_PARM, &mut parm); // only a request; the camera picks the nearest it has
        }
        let mut req = [0u8; 20];
        put_u32(&mut req, 0, BUFFERS);
        put_u32(&mut req, 4, TYPE_CAPTURE);
        put_u32(&mut req, 8, MEMORY_MMAP);
        if !io.call(VIDIOC_REQBUFS, &mut req) {
            return Err((false, "no buffers (another app has the camera?)".into()));
        }
        let mut cap = V4l2Capture { file, format, stride, maps: Vec::new(), stop: Arc::default() };
        for i in 0..u32_at(&req, 0) {
            let io = super::Fd(&cap.file);
            let mut b = buffer(i);
            if !io.call(VIDIOC_QUERYBUF, &mut b) {
                return Err((true, "couldn't ask for a buffer".into()));
            }
            let (offset, length) = (u32_at(&b, 64), u32_at(&b, 72) as usize);
            // SAFETY: mapping the driver's buffer at the offset it gave, unmapped on drop
            let addr = unsafe { libc::mmap(std::ptr::null_mut(), length, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, cap.file.as_raw_fd(), offset as libc::off_t) };
            if addr == libc::MAP_FAILED {
                return Err((true, "couldn't map a buffer".into()));
            }
            cap.maps.push((addr, length));
            if !io.call(VIDIOC_QBUF, &mut b) {
                return Err((true, "couldn't queue a buffer".into()));
            }
        }
        let mut kind = [0u8; 4];
        put_u32(&mut kind, 0, TYPE_CAPTURE);
        if !super::Fd(&cap.file).call(VIDIOC_STREAMON, &mut kind) {
            return Err((true, "the camera wouldn't start".into()));
        }
        Ok(Box::new(cap))
    }

    impl Capture for V4l2Capture {
        fn read(&mut self) -> Result<Vec<u8>, String> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(READ_TIMEOUT_MS as u64);
            loop {
                if self.stop.load(Ordering::SeqCst) {
                    return Err("stopped".into());
                }
                let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: one pollfd for an open descriptor
                let n = unsafe { libc::poll(&mut pfd, 1, 100) };
                if n > 0 && pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    return Err("the camera went away".into());
                }
                if n > 0 {
                    break;
                }
                if std::time::Instant::now() > deadline {
                    return Err("no frame within 5 s".into());
                }
            }
            let io = super::Fd(&self.file);
            let mut b = buffer(0);
            if !io.call(VIDIOC_DQBUF, &mut b) {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let (index, used) = (u32_at(&b, 0) as usize, u32_at(&b, 8) as usize);
            let (addr, length) = *self.maps.get(index).ok_or("a buffer the driver never gave")?;
            // SAFETY: the driver filled used bytes of this mapped buffer and hands it back only after QBUF
            let data = unsafe { std::slice::from_raw_parts(addr as *const u8, used.min(length)) }.to_vec();
            io.call(VIDIOC_QBUF, &mut b);
            if self.format.compressed() {
                return Ok(data);
            }
            Ok(super::tight_rows(&self.format.fourcc, self.format.width, self.format.height, self.stride, &data))
        }

        fn negotiated(&self) -> Option<Choice> {
            Some(self.format.clone())
        }

        fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
            Box::new(Stopper(self.stop.clone()))
        }
    }

    impl Drop for V4l2Capture {
        fn drop(&mut self) {
            let mut kind = [0u8; 4];
            put_u32(&mut kind, 0, TYPE_CAPTURE);
            super::Fd(&self.file).call(VIDIOC_STREAMOFF, &mut kind);
            for (addr, length) in self.maps.drain(..) {
                // SAFETY: mapped in open
                unsafe { libc::munmap(addr, length) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use telescope_core::media::VideoFeed;

    struct FakeCap(Vec<Result<Vec<u8>, String>>);

    struct NoStop;
    impl telescope_core::media::FeedStopper for NoStop {
        fn stop(&self) {}
    }

    impl Capture for FakeCap {
        fn read(&mut self) -> Result<Vec<u8>, String> {
            if self.0.is_empty() { Err("nothing".into()) } else { self.0.remove(0) }
        }
        fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
            Box::new(NoStop)
        }
    }

    fn device() -> Device {
        Device { key: "k".into(), name: "HD Webcam".into(), location: "/dev/video0".into(), modes: Vec::new() }
    }

    struct Rig {
        deps: ReaderDeps,
        reports: Arc<Mutex<Vec<(String, String)>>>,
        located: Arc<Mutex<bool>>,
    }

    fn rig(open: Opener, user: &'static str) -> Rig {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let located = Arc::new(Mutex::new(true));
        let (r, l) = (reports.clone(), located.clone());
        let deps = ReaderDeps {
            locate: Arc::new(move || l.lock().unwrap().then(device)),
            want: Arc::new(|_| Some(Choice { fourcc: "MJPG".into(), width: 1280, height: 720, fps: 30.0 })),
            report: Arc::new(move |s, d| r.lock().unwrap().push((s.into(), d.into()))),
            opener: open,
            users: Arc::new(move |_| user.into()),
            exists: Arc::new(|_| true),
        };
        Rig { deps, reports, located }
    }

    fn frames(list: Vec<Result<Vec<u8>, String>>) -> Opener {
        let list = Arc::new(Mutex::new(Some(list)));
        Arc::new(move |_, _| Ok(Box::new(FakeCap(list.lock().unwrap().take().unwrap_or_default())) as Box<dyn Capture>))
    }

    // test_webcams.py::test_the_reader_hands_out_the_frame_it_checked_first
    #[test]
    fn the_reader_hands_out_the_frame_it_checked_first() {
        let r = rig(frames(vec![Ok(vec![1]), Ok(vec![2])]), "");
        let (choice, mut feed) = open(&r.deps, &Arc::default()).unwrap();
        assert_eq!(feed.read().unwrap().data, [1]);
        assert_eq!(feed.read().unwrap().data, [2]);
        assert_eq!(*r.reports.lock().unwrap(), [("ok".to_string(), "1280×720, MJPEG, 30 fps".to_string())]);
        assert_eq!(cable_bytes(&choice, 1234), 1234);
    }

    // test_webcams.py::test_a_raw_frame_counts_what_crossed_the_cable_not_the_decoded_size
    #[test]
    fn a_raw_frame_counts_what_crossed_the_cable() {
        let raw = Choice { fourcc: "YUYV".into(), width: 640, height: 480, fps: 30.0 };
        assert_eq!(cable_bytes(&raw, 0), 640 * 480 * 2);
    }

    // test_webcams.py::test_mjpeg_frames_are_decoded_by_the_streams_decoders
    #[test]
    fn mjpeg_goes_to_the_streams_decoders_and_raw_to_our_own() {
        assert!(is_jpeg(&Choice { fourcc: "MJPG".into(), width: 2, height: 2, fps: 30.0 }));
        let mut d = RawDecoder { fourcc: "YUYV".into(), width: 2, height: 2 };
        let out = d.decode(&Packet { data: vec![10, 100, 20, 200, 30, 101, 40, 201], time_ns: 0 }).newest.unwrap();
        assert_eq!(&out.data[..], &[10, 20, 30, 40, 100, 200]);
        assert!(d.decode(&Packet { data: vec![1, 2], time_ns: 0 }).newest.is_none());
    }

    #[test]
    fn padded_rows_from_the_driver_are_packed_tight() {
        // 2x2 YUYV with 4-byte rows padded to 6
        let padded = [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        assert_eq!(tight_rows("YUYV", 2, 2, 6, &padded), (1..=8).collect::<Vec<u8>>());
        // 2x2 YU12 with a 4-byte stride: Y rows of 2 at 4, chroma rows of 1 at 2
        let planar = [1, 2, 0, 0, 3, 4, 0, 0, 5, 0, 6, 0];
        assert_eq!(tight_rows("YU12", 2, 2, 4, &planar), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(tight_rows("YUYV", 2, 2, 4, &[1, 2, 3, 4, 5, 6, 7, 8]), (1..=8).collect::<Vec<u8>>());
    }

    #[test]
    fn raw_frames_decode_at_the_size_the_driver_gave() {
        struct Adjusted(FakeCap);
        impl Capture for Adjusted {
            fn read(&mut self) -> Result<Vec<u8>, String> {
                self.0.read()
            }
            fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
                self.0.stopper()
            }
            fn negotiated(&self) -> Option<Choice> {
                Some(Choice { fourcc: "YUYV".into(), width: 2, height: 2, fps: 30.0 })
            }
        }
        let r = rig(Arc::new(|_, _| Ok(Box::new(Adjusted(FakeCap(vec![Ok(vec![0; 8])]))) as Box<dyn Capture>)), "");
        let (choice, _) = open(&r.deps, &Arc::new(Mutex::new(String::new()))).unwrap();
        assert_eq!((choice.width, choice.height), (2, 2));
    }

    // test_webcams.py::test_an_unplugged_webcam_waits_to_be_plugged_back_in
    #[test]
    fn an_unplugged_webcam_waits_to_be_plugged_back_in() {
        let r = rig(frames(vec![]), "");
        *r.located.lock().unwrap() = false;
        let waiting = Arc::new(Mutex::new(String::new()));
        assert!(open(&r.deps, &waiting).is_err());
        assert_eq!(*waiting.lock().unwrap(), GONE_TEXT);
        assert_eq!(r.reports.lock().unwrap()[0].0, "gone");
    }

    // test_webcams.py::test_a_webcam_another_program_holds_says_which
    #[test]
    fn a_webcam_another_program_holds_says_which() {
        let r = rig(Arc::new(|_, _| Err((true, "busy".into()))), "zoom");
        let waiting = Arc::new(Mutex::new(String::new()));
        assert!(open(&r.deps, &waiting).is_err());
        assert_eq!(*waiting.lock().unwrap(), BUSY_TEXT);
        assert_eq!(r.reports.lock().unwrap()[0], ("busy".to_string(), "zoom".to_string()));
    }

    // test_webcams.py::test_a_webcam_that_wont_open_counts_as_busy
    #[test]
    fn a_webcam_that_wont_open_counts_as_busy() {
        let r = rig(Arc::new(|_, _| Err((false, "refused".into()))), "");
        assert!(open(&r.deps, &Arc::default()).is_err());
        assert_eq!(r.reports.lock().unwrap()[0], ("busy".to_string(), String::new()));
    }

    // test_webcams.py::test_an_open_webcam_that_sends_nothing_says_so
    #[test]
    fn an_open_webcam_that_sends_nothing_says_so() {
        let r = rig(frames(vec![]), "");
        let waiting = Arc::new(Mutex::new(String::new()));
        assert!(open(&r.deps, &waiting).is_err());
        assert_eq!(*waiting.lock().unwrap(), NO_FRAMES_TEXT);
        assert_eq!(r.reports.lock().unwrap()[0].0, "silent");
    }

    // test_webcams.py::test_unplugging_mid_stream_drops_the_stream_and_says_so
    #[test]
    fn unplugging_mid_stream_drops_the_stream_and_says_so() {
        let r = rig(frames(vec![Ok(vec![1])]), "");
        let waiting = Arc::new(Mutex::new(String::new()));
        let (_, mut feed) = open(&r.deps, &waiting).unwrap();
        feed.read().unwrap();
        *r.located.lock().unwrap() = false;
        assert!(feed.read().is_err());
        assert_eq!(*waiting.lock().unwrap(), GONE_TEXT);
        assert_eq!(r.reports.lock().unwrap().last().unwrap().0, "gone");
    }

    #[test]
    fn raw_formats_turn_into_i420() {
        assert_eq!(raw_to_i420("UYVY", 2, 2, &[100, 10, 200, 20, 101, 30, 201, 40]).unwrap().data[..4], [10, 20, 30, 40]);
        let nv12 = raw_to_i420("NV12", 2, 2, &[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(&nv12.data[..], &[1, 2, 3, 4, 5, 6]);
        let yv12 = raw_to_i420("YV12", 2, 2, &[1, 2, 3, 4, 6, 5]).unwrap();
        assert_eq!(&yv12.data[4..], &[5, 6]);
        assert!(raw_to_i420("H264", 2, 2, &[0; 16]).is_none());
    }
}
