//! Windows webcams through Media Foundation (webcams.scan_windows and open_capture). Python listed them with Qt
//! Multimedia, which reads Media Foundation itself, and read them with OpenCV's DirectShow backend; here both go through
//! Media Foundation: MFEnumDeviceSources lists, each camera's media types give its modes, and a source reader set to the
//! picked native type hands out its frames. A camera's key and location are its symbolic link, which is what Qt called
//! its id, so saved choices carry over.
//!
//! What a camera's media types mean and which one to ask for are plain functions, so tests run them anywhere.

use crate::webcams::{Choice, Mode, FPS_STEPS};

/// Windows adds this to the names of cameras made with MFCreateVirtualCamera.
const VIRTUAL_SUFFIX: &str = " (Windows Virtual Camera)";
/// 29.97 counts as 30.
const FPS_SLACK: f64 = 0.5;
/// More raw video per second than this (about what USB 2 carries) isn't coming off the camera as raw.
const RAW_LIMIT_BITS: f64 = 300e6;

/// One media type a camera offers, as Media Foundation describes it: its subtype's FOURCC, size and frame rate range.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaType {
    pub subtype: String,
    pub width: u32,
    pub height: u32,
    pub fps_min: f64,
    pub fps_max: f64,
}

/// A video subtype GUID's FOURCC: Media Foundation's are the code in Data1 on a fixed base.
pub fn subtype_fourcc(data1: u32) -> String {
    crate::webcams::fourcc(data1)
}

/// The fourcc a subtype goes by here (YUY2 is V4L2's YUYV), or None for one the raw decoder can't read.
pub fn our_fourcc(subtype: &str) -> Option<&'static str> {
    match subtype {
        "MJPG" => Some("MJPG"),
        "YUY2" => Some("YUYV"),
        "UYVY" => Some("UYVY"),
        "NV12" => Some("NV12"),
        _ => None,
    }
}

/// The modes a camera's media types give (_qt_modes): one rate as it is (59.94 stays 59.94), a range as the usual
/// rates inside it. Media Foundation offers an NV12 copy of each MJPEG mode that it decodes itself, which isn't what
/// the camera sends, so those go.
pub fn modes(types: &[MediaType]) -> Vec<Mode> {
    let mut out: Vec<Mode> = Vec::new();
    for t in types {
        let Some(fourcc) = our_fourcc(&t.subtype) else { continue };
        let (lo, hi) = (t.fps_min.min(t.fps_max), t.fps_max);
        let fps: Vec<f64> = if hi - lo < 1.0 {
            if hi > 0.0 { vec![(hi * 100.0).round() / 100.0] } else { Vec::new() }
        } else {
            FPS_STEPS.iter().map(|&f| f as f64).filter(|&f| lo - FPS_SLACK <= f && f <= hi + FPS_SLACK).collect()
        };
        let mode = Mode::new(fourcc, t.width, t.height, &fps);
        if !out.contains(&mode) {
            out.push(mode);
        }
    }
    let jpeg: Vec<(u32, u32, Vec<f64>)> = out.iter().filter(|m| m.fourcc == "MJPG").map(|m| (m.width, m.height, m.fps.clone())).collect();
    out.retain(|m| !(m.fourcc == "NV12" && jpeg.contains(&(m.width, m.height, m.fps.clone()))));
    // Frame Server also offers raw copies it decodes from MJPEG at rates no cable could carry raw; MJPEG itself is the real mode
    let carried = |m: &Mode| (m.width as f64) * (m.height as f64) * 16.0 * m.max_fps() <= RAW_LIMIT_BITS;
    let jpeg: Vec<(u32, u32, f64)> = out.iter().filter(|m| m.fourcc == "MJPG").map(|m| (m.width, m.height, m.max_fps())).collect();
    out.retain(|m| m.fourcc == "MJPG" || m.fps.is_empty() || carried(m)
        || !jpeg.iter().any(|&(w, h, f)| (w, h) == (m.width, m.height) && f + FPS_SLACK >= m.max_fps()));
    out.sort_by(|a, b| (&a.fourcc, a.width, a.height).cmp(&(&b.fourcc, b.width, b.height))
        .then(a.fps.partial_cmp(&b.fps).unwrap_or(std::cmp::Ordering::Equal)));
    out
}

/// Which of a camera's native media types to ask for choice: the right format and size whose rates take it, the one
/// whose top rate is nearest when several do. None when none fits.
pub fn pick(types: &[MediaType], choice: &Choice) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, t) in types.iter().enumerate() {
        if our_fourcc(&t.subtype) != Some(choice.fourcc.as_str()) || (t.width, t.height) != (choice.width, choice.height) {
            continue;
        }
        let takes = choice.fps <= 0.0 || (t.fps_min.min(t.fps_max) - FPS_SLACK <= choice.fps && choice.fps <= t.fps_max + FPS_SLACK);
        if !takes {
            continue;
        }
        let off = (t.fps_max - choice.fps).abs();
        if best.is_none_or(|(_, b)| off < b) {
            best = Some((i, off));
        }
    }
    best.map(|(i, _)| i)
}

/// A frame rate as Media Foundation's ratio: whole rates over 1, others (29.97) to a thousandth.
pub fn rate_ratio(fps: f64) -> (u32, u32) {
    if (fps - fps.round()).abs() < 1e-6 { (fps.round() as u32, 1) } else { ((fps * 1000.0).round() as u32, 1000) }
}

/// What a camera is called in lists, without the suffix Windows gives virtual cameras.
pub fn display_name(name: &str) -> String {
    crate::webcams::tidy_name(name.strip_suffix(VIRTUAL_SUFFIX).unwrap_or(name))
}

/// A device path without its interface class (the "#{guid}" at the end), which DirectShow and Media Foundation give
/// differently for one camera (webcams_win.device_instance).
pub fn device_instance(path: &str) -> String {
    let path = path.trim().to_lowercase();
    match path.rfind("#{") {
        Some(cut) if cut > 0 => path[..cut].to_string(),
        _ => path,
    }
}

/// Why a camera that opened won't send, from Media Foundation's error: Some(true) when another app has it, so the card
/// says busy rather than silent.
pub fn held_elsewhere(hresult: i32) -> bool {
    matches!(hresult as u32,
        0xC00D_3EA2 // MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED: the device was taken or lost
        | 0xC00D_3EA3 // MF_E_VIDEO_RECORDING_DEVICE_PREEMPTED
        | 0xC00D_3704 // MF_E_HW_MFT_FAILED_START_STREAMING
        | 0x8007_0020 // ERROR_SHARING_VIOLATION
        | 0x8007_00AA) // ERROR_BUSY
}

/// Windows (its camera privacy switch) or security software said no to Telescope, which is not another app holding the camera.
pub fn access_denied(hresult: i32) -> bool {
    matches!(hresult as u32,
        0x8007_0005 // E_ACCESSDENIED
        | 0x8007_04EC) // ERROR_ACCESS_DISABLED_BY_POLICY
}

/// The first HRESULT written as 0x and eight hex digits in an error's text, which is how the windows crate prints one.
pub fn hresult_in(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    lower.match_indices("0x").find_map(|(at, _)| {
        let digits = lower.get(at + 2..at + 10)?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok().map(|v| v as i32)
    })
}

/// Whether an open's error text says access was denied.
pub fn denied_in(text: &str) -> bool {
    hresult_in(text).is_some_and(access_denied)
}

#[cfg(windows)]
pub use real::{device_source, open, present, WindowsScanner};

#[cfg(windows)]
mod real {
    use super::*;
    use crate::webcam_capture::Capture;
    use crate::webcams::{is_virtual, tell_apart, Device};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, Once};
    use std::time::{Duration, Instant};
    use windows::core::{GUID, HSTRING, PWSTR};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

    /// How long a read waits for a frame before the camera counts as silent.
    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    /// Windows N has none of these unless its media pack is added; the program loads them late (app/build.rs), so that costs webcams and nothing else.
    fn media_foundation_here() -> bool {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LoadLibraryW(name: *const u16) -> isize;
        }
        static HERE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *HERE.get_or_init(|| {
            ["mfplat.dll", "mf.dll", "mfreadwrite.dll"].iter().all(|name| {
                // SAFETY: a NUL-terminated name; staying loaded is what's wanted
                unsafe { LoadLibraryW(crate::win::wide(name).as_ptr()) != 0 }
            })
        })
    }

    fn start() {
        static STARTED: Once = Once::new();
        // SAFETY: COM on this thread (an STA already set up by the window stays one, which Media Foundation takes too)
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        STARTED.call_once(|| {
            // SAFETY: once per process, never shut down
            if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
                log::warn!("Media Foundation didn't start: {e}");
            }
        });
    }

    fn video_attributes(link: Option<&str>) -> windows::core::Result<IMFAttributes> {
        let mut attrs = None;
        // SAFETY: attrs is filled on success; the GUIDs are statics
        unsafe {
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
            attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
            if let Some(link) = link {
                attrs.SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &HSTRING::from(link))?;
            }
            Ok(attrs)
        }
    }

    fn string(activate: &IMFActivate, key: &GUID) -> Option<String> {
        let mut text = PWSTR::null();
        let mut len = 0u32;
        // SAFETY: Media Foundation allocates text, freed here once copied
        unsafe {
            activate.GetAllocatedString(key, &mut text, &mut len).ok()?;
            let out = text.to_string().ok();
            CoTaskMemFree(Some(text.0 as *const _));
            out
        }
    }

    /// (name, symbolic link) of every camera Media Foundation lists, and its activator.
    fn cameras() -> Vec<(String, String, IMFActivate)> {
        if !media_foundation_here() {
            return Vec::new();
        }
        start();
        let Ok(attrs) = video_attributes(None) else { return Vec::new() };
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: Media Foundation allocates the array; each entry is taken out once and the array freed
        unsafe {
            if MFEnumDeviceSources(&attrs, &mut list, &mut count).is_err() || list.is_null() {
                return Vec::new();
            }
            let mut out = Vec::new();
            for i in 0..count as usize {
                if let Some(activate) = std::ptr::read(list.add(i)) {
                    let name = string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
                    let link = string(&activate, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK).unwrap_or_default();
                    out.push((name, link, activate));
                }
            }
            CoTaskMemFree(Some(list as *const _));
            out
        }
    }

    fn media_type(t: &IMFMediaType) -> Option<MediaType> {
        // SAFETY: plain attribute reads
        unsafe {
            let subtype = t.GetGUID(&MF_MT_SUBTYPE).ok()?;
            let size = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
            let ratio = |v: u64| { let d = (v & 0xFFFF_FFFF) as f64; if d > 0.0 { (v >> 32) as f64 / d } else { 0.0 } };
            let rate = t.GetUINT64(&MF_MT_FRAME_RATE).map(ratio).unwrap_or(0.0);
            let lo = t.GetUINT64(&MF_MT_FRAME_RATE_RANGE_MIN).map(ratio).unwrap_or(rate);
            let hi = t.GetUINT64(&MF_MT_FRAME_RATE_RANGE_MAX).map(ratio).unwrap_or(rate);
            Some(MediaType { subtype: subtype_fourcc(subtype.data1), width: (size >> 32) as u32, height: size as u32, fps_min: lo, fps_max: hi })
        }
    }

    /// The media types a camera's first video stream offers.
    fn types_of(source: &IMFMediaSource) -> Vec<MediaType> {
        // SAFETY: COM calls on live objects
        unsafe {
            let Ok(pd) = source.CreatePresentationDescriptor() else { return Vec::new() };
            let mut selected = windows::core::BOOL::default();
            let mut sd = None;
            if pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd).is_err() {
                return Vec::new();
            }
            let Some(Ok(handler)) = sd.map(|s| s.GetMediaTypeHandler()) else { return Vec::new() };
            let count = handler.GetMediaTypeCount().unwrap_or(0);
            (0..count).filter_map(|i| handler.GetMediaTypeByIndex(i).ok()).filter_map(|t| media_type(&t)).collect()
        }
    }

    /// Lists webcams, asking each new one what it offers once (opening a camera can light its LED, so not every scan).
    #[derive(Default)]
    pub struct WindowsScanner {
        known: HashMap<String, Vec<Mode>>,
    }

    impl WindowsScanner {
        pub fn scan(&mut self) -> Vec<Device> {
            let mut found = Vec::new();
            let mut known = HashMap::new();
            for (name, link, activate) in cameras() {
                let name = display_name(&name);
                if link.is_empty() || is_virtual(&name, "") {
                    continue; // Telescope's own camera and other virtual ones, so a stream can't feed back into itself
                }
                // A camera that gave no modes (blocked by security software, held by an app) is asked again next scan
                let modes = match self.known.remove(&link).filter(|m| !m.is_empty()) {
                    Some(modes) => modes,
                    None => {
                        // SAFETY: the source is shut down once read, as the activator is
                        unsafe {
                            let modes = match activate.ActivateObject::<IMFMediaSource>() {
                                Ok(s) => {
                                    let modes = super::modes(&types_of(&s));
                                    let _ = s.Shutdown();
                                    modes
                                }
                                Err(e) => {
                                    log::info!("Couldn't ask {name} for its modes: {e}");
                                    Vec::new()
                                }
                            };
                            let _ = activate.ShutdownObject();
                            modes
                        }
                    }
                };
                if !modes.is_empty() {
                    known.insert(link.clone(), modes.clone());
                }
                found.push(Device { key: link.clone(), name, location: link, modes });
            }
            self.known = known;
            tell_apart(found)
        }
    }

    /// A fresh media source for the camera at link, for reading and setting its picture controls.
    pub fn device_source(link: &str) -> windows::core::Result<IMFMediaSource> {
        if !media_foundation_here() {
            return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
        }
        start();
        // SAFETY: COM calls on objects made here
        unsafe { MFCreateDeviceSource(&video_attributes(Some(link))?) }
    }

    /// Whether a camera with this symbolic link is still plugged in.
    pub fn present(link: &str) -> bool {
        let want = device_instance(link);
        cameras().iter().any(|(_, l, _)| device_instance(l) == want)
    }

    /// The source, shared with the watchdog and the stopper: Media Foundation's objects are free-threaded.
    struct Source(IMFMediaSource);
    // SAFETY: media sources may be called from any thread (MTA objects)
    unsafe impl Send for Source {}
    unsafe impl Sync for Source {}

    struct Watch {
        source: Source,
        /// When the read now waiting began.
        reading: Mutex<Option<Instant>>,
        done: AtomicBool,
        timed_out: AtomicBool,
    }

    pub struct MfCapture {
        reader: IMFSourceReader,
        watch: Arc<Watch>,
        first: Option<Vec<u8>>,
        negotiated: Option<Choice>,
    }

    // SAFETY: the reader is only used from the thread reading the feed; Media Foundation's objects are free-threaded
    unsafe impl Send for MfCapture {}

    struct Stopper(Arc<Watch>);

    impl telescope_core::media::FeedStopper for Stopper {
        fn stop(&self) {
            self.0.done.store(true, Ordering::SeqCst);
            // SAFETY: shutting the source down makes a waiting ReadSample return
            unsafe {
                let _ = self.0.source.0.Shutdown();
            }
        }
    }

    impl MfCapture {
        fn read_sample(&mut self) -> Result<Vec<u8>, (bool, String)> {
            loop {
                if self.watch.done.load(Ordering::SeqCst) {
                    return Err((false, "stopped".into()));
                }
                let (mut flags, mut sample) = (0u32, None);
                *self.watch.reading.lock().unwrap() = Some(Instant::now());
                // SAFETY: blocks until a frame, an error or the source shutting down
                let got = unsafe { self.reader.ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample)) };
                *self.watch.reading.lock().unwrap() = None;
                if self.watch.timed_out.load(Ordering::SeqCst) {
                    return Err((false, "no frame within 5 s".into()));
                }
                if let Err(e) = got {
                    return Err((held_elsewhere(e.code().0), e.to_string()));
                }
                if flags & (MF_SOURCE_READERF_ERROR.0 as u32 | MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                    return Err((false, format!("the camera stopped (flags {flags:#x})")));
                }
                let Some(sample) = sample else { continue }; // a gap in the stream, not a frame
                // SAFETY: the buffer is locked while its bytes are copied out
                return unsafe {
                    let buffer = sample.ConvertToContiguousBuffer().map_err(|e| (false, e.to_string()))?;
                    let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
                    buffer.Lock(&mut data, None, Some(&mut len)).map_err(|e| (false, e.to_string()))?;
                    let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
                    let _ = buffer.Unlock();
                    Ok(bytes)
                };
            }
        }
    }

    impl Capture for MfCapture {
        fn read(&mut self) -> Result<Vec<u8>, String> {
            match self.first.take() {
                Some(first) => Ok(first),
                None => self.read_sample().map_err(|(_, why)| why),
            }
        }

        fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
            Box::new(Stopper(self.watch.clone()))
        }

        fn negotiated(&self) -> Option<Choice> {
            self.negotiated.clone()
        }
    }

    impl Drop for MfCapture {
        fn drop(&mut self) {
            self.watch.done.store(true, Ordering::SeqCst);
            // SAFETY: the source is ours to shut down
            unsafe {
                let _ = self.watch.source.0.Shutdown();
            }
        }
    }

    /// Ends a read that waits too long, so a camera that opened but sends nothing counts as silent.
    fn watchdog(watch: Arc<Watch>) {
        let _ = std::thread::Builder::new().name("webcam-watchdog".into()).spawn(move || {
            while !watch.done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
                let late = watch.reading.lock().unwrap().is_some_and(|since| since.elapsed() > READ_TIMEOUT);
                if late {
                    watch.timed_out.store(true, Ordering::SeqCst);
                    // SAFETY: shutting the source down makes the waiting ReadSample return
                    unsafe {
                        let _ = watch.source.0.Shutdown();
                    }
                    return;
                }
            }
        });
    }

    /// Opens the camera at location (its symbolic link) on the native type that fits choice and reads a first frame,
    /// so one another app holds says so: Err((opened at all, why)) as webcam_capture's Opener wants.
    pub fn open(location: &str, choice: &Choice) -> Result<Box<dyn Capture>, (bool, String)> {
        if !media_foundation_here() {
            return Err((false, "Media Foundation isn't installed".into()));
        }
        start();
        let fail = |e: windows::core::Error| (false, e.to_string());
        // SAFETY: COM calls on objects made here
        let (source, reader, negotiated) = unsafe {
            let attrs = video_attributes(Some(location)).map_err(fail)?;
            let source: IMFMediaSource = MFCreateDeviceSource(&attrs).map_err(fail)?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, None::<&IMFAttributes>).map_err(|e| {
                let _ = source.Shutdown();
                (false, e.to_string())
            })?;
            let natives: Vec<(IMFMediaType, MediaType)> = (0..)
                .map_while(|i| reader.GetNativeMediaType(STREAM, i).ok())
                .filter_map(|t| media_type(&t).map(|m| (t, m)))
                .collect();
            let described: Vec<MediaType> = natives.iter().map(|(_, m)| m.clone()).collect();
            if let Some(i) = pick(&described, choice) {
                let t = &natives[i].0;
                if choice.fps > 0.0 {
                    let (num, den) = rate_ratio(choice.fps);
                    let _ = t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(num) << 32) | u64::from(den));
                }
                if let Err(e) = reader.SetCurrentMediaType(STREAM, None, t) {
                    log::info!("The webcam refused {}: {e}", choice.describe());
                }
            } else {
                log::info!("The webcam offers nothing like {}; reading what it gives", choice.describe());
            }
            let negotiated = reader.GetCurrentMediaType(STREAM).ok().and_then(|t| media_type(&t)).and_then(|m| {
                let fourcc = our_fourcc(&m.subtype)?;
                Some(Choice { fourcc: fourcc.into(), width: m.width, height: m.height, fps: m.fps_max })
            });
            (source, reader, negotiated)
        };
        let watch = Arc::new(Watch { source: Source(source), reading: Mutex::new(None), done: AtomicBool::new(false), timed_out: AtomicBool::new(false) });
        watchdog(watch.clone());
        let mut capture = MfCapture { reader, watch, first: None, negotiated };
        match capture.read_sample() {
            Ok(first) => {
                capture.first = Some(first);
                Ok(Box::new(capture))
            }
            // Held by another app: busy rather than silent
            Err((true, why)) => Err((false, why)),
            Err((false, why)) => Err((true, why)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(subtype: &str, w: u32, h: u32, lo: f64, hi: f64) -> MediaType {
        MediaType { subtype: subtype.into(), width: w, height: h, fps_min: lo, fps_max: hi }
    }

    #[test]
    fn media_types_become_modes_as_qt_listed_them() {
        let got = modes(&[
            t("YUY2", 640, 480, 30.0, 30.0),
            t("MJPG", 1280, 720, 5.0, 30.0),
            // Media Foundation's own NV12 copy of that MJPEG mode
            t("NV12", 1280, 720, 5.0, 30.0),
            t("NV12", 640, 480, 59.94, 59.94),
            t("YUY2", 640, 480, 30.0, 30.0),
            t("RGB3", 640, 480, 30.0, 30.0),
            t("YUY2", 320, 240, 0.0, 0.0),
        ]);
        assert_eq!(got, [
            Mode::new("MJPG", 1280, 720, &[5.0, 10.0, 15.0, 20.0, 24.0, 25.0, 30.0]),
            Mode::new("NV12", 640, 480, &[59.94]),
            Mode::new("YUYV", 320, 240, &[]),
            Mode::new("YUYV", 640, 480, &[30.0]),
        ]);
    }

    #[test]
    fn raw_copies_decoded_from_mjpeg_at_impossible_rates_are_not_modes() {
        let got = modes(&[
            t("MJPG", 1920, 1080, 30.0, 30.0),
            t("YUY2", 1920, 1080, 30.0, 30.0),
            t("YUY2", 1920, 1080, 5.0, 5.0),
            t("MJPG", 640, 480, 30.0, 30.0),
            t("YUY2", 640, 480, 30.0, 30.0),
            t("YUY2", 1600, 896, 30.0, 30.0),
        ]);
        let has = |f: &str, w: u32, fps: f64| got.iter().any(|m| m.fourcc == f && m.width == w && m.fps.contains(&fps));
        assert!(has("MJPG", 1920, 30.0) && !has("YUYV", 1920, 30.0), "raw 1080p at 30 isn't the camera's");
        assert!(has("YUYV", 1920, 5.0), "a real slow raw mode stays");
        assert!(has("YUYV", 640, 30.0), "raw that fits the cable stays");
        assert!(has("YUYV", 1600, 30.0), "raw with no MJPEG twin stays");
    }

    #[test]
    fn the_native_type_asked_for_fits_format_size_and_rate() {
        let types = [t("NV12", 1280, 720, 30.0, 30.0), t("MJPG", 1280, 720, 15.0, 15.0), t("MJPG", 1280, 720, 30.0, 30.0),
                     t("MJPG", 1920, 1080, 5.0, 30.0)];
        let c = |f: &str, w, h, fps| Choice { fourcc: f.into(), width: w, height: h, fps };
        assert_eq!(pick(&types, &c("MJPG", 1280, 720, 30.0)), Some(2));
        assert_eq!(pick(&types, &c("MJPG", 1280, 720, 15.0)), Some(1));
        assert_eq!(pick(&types, &c("MJPG", 1920, 1080, 29.97)), Some(3), "a range takes a rate inside it");
        assert_eq!(pick(&types, &c("YUYV", 1280, 720, 30.0)), None);
        assert_eq!(pick(&types, &c("MJPG", 640, 480, 30.0)), None);
        assert_eq!(rate_ratio(30.0), (30, 1));
        assert_eq!(rate_ratio(29.97), (29970, 1000));
    }

    #[test]
    fn names_paths_and_errors() {
        assert_eq!(display_name("Telescope (Windows Virtual Camera)"), "Telescope");
        assert_eq!(subtype_fourcc(0x3259_5559), "YUY2");
        let mf = r"\\?\USB#VID_046D&PID_085B&MI_00#7&2a1b3c&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        let ds = r"\\?\usb#vid_046d&pid_085b&mi_00#7&2a1b3c&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
        assert_eq!(device_instance(mf), device_instance(ds));
        assert!(held_elsewhere(0xC00D_3EA2_u32 as i32));
        assert!(!held_elsewhere(0x8000_4005_u32 as i32), "E_FAIL is just a failure");
    }

    #[test]
    fn a_denied_camera_is_not_a_busy_one() {
        assert!(access_denied(0x8007_0005_u32 as i32) && !held_elsewhere(0x8007_0005_u32 as i32));
        assert!(held_elsewhere(0x8007_0020_u32 as i32) && !access_denied(0x8007_0020_u32 as i32));
        assert_eq!(hresult_in("Access is denied. (0x80070005)"), Some(0x8007_0005_u32 as i32));
        assert_eq!(hresult_in("Access is denied. (0X8007000A)"), Some(0x8007_000A_u32 as i32));
        assert_eq!(hresult_in("no code here, 0x12"), None);
        assert!(denied_in("Access is denied. (0x80070005)"));
        assert!(!denied_in("the camera stopped (flags 0x4)"));
        assert!(!denied_in("The device is in use. (0xC00D3EA3)"));
    }
}
