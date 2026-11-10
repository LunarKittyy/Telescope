//! UnityCapture, the camera older Windows gets: frames go to the shared image an app's filter makes when it opens the
//! camera, as RGBA bottom row first, the way pyvirtualcam's unitycapture backend sends them (UnityCapture's shared.inl).

use crate::convert::{fit_frame, i420_to_rgba_flipped_into, rgb_to_i420};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::output::VideoOutput;

/// The biggest picture the filter makes room for: 4K at four 16-bit channels.
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
}

/// The picture last made, and what it was made from: the same pixels again (a wait screen showing one picture) need no new conversion.
struct Made {
    /// Weak, so keeping it doesn't make a step that changes the pixels copy them (and its allocation can't be reused while it's here).
    source: std::sync::Weak<Vec<u8>>,
    format: PixelFormat,
    size: (u32, u32),
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
}

impl UcCamera {
    pub fn new(width: u32, height: u32, device: String, target: Box<dyn Target>, moved: Box<dyn Fn() -> bool + Send>) -> UcCamera {
        UcCamera { width, height, device, target, moved, rgba: Vec::new(), made: None }
    }
}

impl VideoOutput for UcCamera {
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Rgba) {
            return Err(format!("UnityCapture takes I420 or RGBA frames, not {:?}", frame.format));
        }
        if !self.target.ready() {
            return Ok(());
        }
        let same = self.made.as_ref().is_some_and(|m| std::ptr::eq(m.source.as_ptr(), std::sync::Arc::as_ptr(&frame.data)) && m.format == frame.format && m.size == (frame.width, frame.height));
        if !same {
            self.made = None;
            picture_into(frame, self.width, self.height, &mut self.rgba)?;
            self.made = Some(Made { source: std::sync::Arc::downgrade(&frame.data), format: frame.format, size: (frame.width, frame.height) });
        }
        self.target.send(&self.rgba, self.width, self.height)
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
    open: Option<(crate::win::Owned, crate::win::Owned, crate::win::Owned, crate::win::View)>,
}

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
        Shared { num, want: None, open: None }
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
                let want = self.want.take().unwrap();
                self.open = Some((mutex, want, sent, view));
                true
            }
            None => false,
        }
    }
}

#[cfg(windows)]
impl Target for Shared {
    fn ready(&mut self) -> bool {
        self.try_open()
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
        ready: bool,
        sent: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl Target for Sink {
        fn send(&mut self, rgba: &[u8], _width: u32, _height: u32) -> Result<(), String> {
            self.sent.lock().unwrap().push(rgba.to_vec());
            Ok(())
        }
        fn ready(&mut self) -> bool {
            self.ready
        }
    }

    fn camera(ready: bool) -> (UcCamera, std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (UcCamera::new(4, 4, "cam".into(), Box::new(Sink { ready, sent: sent.clone() }), Box::new(|| false)), sent)
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
