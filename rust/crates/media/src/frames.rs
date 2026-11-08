//! Frames from other streams (HostFrames): the newest picture of every running stream, for plugins that combine them.
//! The window says which streams run and plugins ask from any thread, often from inside another stream's frame steps.
//! A stream swaps in its newest frame whole and never changes it after, so handing one out shares it: no copy.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, PixelFormat, StreamFrame};
use telescope_core::host::FrameStore;
use telescope_core::media::{FrameSlot, StreamFrames};
use telescope_core::time::monotonic_ns;

/// Every frame a stream sends out gets the next number, across all streams, so a number never comes round again.
static FRAME_NUMBERS: AtomicU64 = AtomicU64::new(1);

/// One stream's newest frames (StreamWorker's _published and _latest).
#[derive(Default)]
pub struct Slot {
    state: Mutex<SlotState>,
    changed: Condvar,
    /// The small copy of a frame before the view, and when it was asked for and last made (monotonic ns, 0 for never).
    raw: RawStill,
}

/// How long after the last raw_still the stream keeps offering frames, and the least time between two copies (about 5 a second).
pub const RAW_KEEP_NS: u64 = 1_500_000_000;
pub const RAW_EVERY_NS: u64 = 190_000_000;
/// The widest a still is kept.
pub const RAW_WIDTH: usize = 320;

#[derive(Default)]
struct RawStill {
    asked: AtomicU64,
    made: AtomicU64,
    still: Mutex<Option<Frame>>,
}

#[derive(Default)]
struct SlotState {
    published: Option<Frame>,
    latest: Option<Frame>,
    version: u64,
}

impl Slot {
    pub fn new() -> Arc<Slot> {
        Arc::new(Slot::default())
    }
}

impl FrameSlot for Slot {
    fn publish(&self, mut frame: Frame) -> u64 {
        let mut st = self.state.lock().unwrap();
        // Numbered under the lock, so a stream's numbers only go up in the order its frames come out
        let number = FRAME_NUMBERS.fetch_add(1, Ordering::SeqCst);
        frame.number = number;
        frame.time_ns = monotonic_ns();
        st.published = Some(frame.clone());
        st.latest = Some(frame);
        st.version += 1;
        self.changed.notify_all();
        number
    }

    fn published(&self) -> Option<Frame> {
        self.state.lock().unwrap().published.clone()
    }

    fn replace(&self, mut frame: Frame, number: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.latest.is_none() || st.published.as_ref().map(|f| f.number) != Some(number) {
            return false;
        }
        // Still the frame that went out under this number, so a stream counting what it sends doesn't take it for a new one
        frame.number = number;
        st.latest = Some(frame);
        st.version += 1;
        self.changed.notify_all();
        true
    }

    fn latest(&self) -> Option<(Frame, u64)> {
        let st = self.state.lock().unwrap();
        st.latest.clone().map(|f| (f, st.version))
    }

    fn raw_still(&self) -> Option<Frame> {
        self.raw.asked.store(monotonic_ns().max(1), Ordering::Relaxed);
        self.raw.still.lock().unwrap().clone()
    }

    fn wants_raw(&self) -> bool {
        let (asked, now) = (self.raw.asked.load(Ordering::Relaxed), monotonic_ns());
        asked != 0 && now.saturating_sub(asked) < RAW_KEEP_NS && now.saturating_sub(self.raw.made.load(Ordering::Relaxed)) >= RAW_EVERY_NS
    }

    fn offer_raw(&self, frame: &Frame) {
        let Some(rgb) = telescope_core::image::frame_rgb(frame) else { return };
        let (w, h) = (frame.width as usize, frame.height as usize);
        if w == 0 || h == 0 || rgb.len() < w * h * 3 {
            return;
        }
        let w2 = w.min(RAW_WIDTH);
        let h2 = (h * w2).div_ceil(w).max(1);
        let small = if (w2, h2) == (w, h) { rgb } else { telescope_core::image::resize_area(&rgb, w, h, 3, w2, h2) };
        let mut rgba = Vec::with_capacity(w2 * h2 * 4);
        for px in small.chunks_exact(3) {
            rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        *self.raw.still.lock().unwrap() = Some(Frame::new(w2 as u32, h2 as u32, PixelFormat::Rgba, rgba));
        self.raw.made.store(monotonic_ns().max(1), Ordering::Relaxed);
    }

    fn wait_newer(&self, seen: u64, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        let mut st = self.state.lock().unwrap();
        while st.version <= seen {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            st = self.changed.wait_timeout(st, left).unwrap().0;
        }
        true
    }
}

type Streams = Arc<Vec<(String, String, Arc<dyn FrameSlot>)>>;

/// Which streams run, and their newest frames (frames.Frames).
#[derive(Default)]
pub struct Frames {
    // Replaced whole, so readers never see it half done
    streams: RwLock<Streams>,
}

impl Frames {
    pub fn new() -> Arc<Frames> {
        Arc::new(Frames::default())
    }

    fn streams(&self) -> Streams {
        self.streams.read().unwrap().clone()
    }
}

fn stream_frame(entry: &(String, String, Arc<dyn FrameSlot>)) -> Option<StreamFrame> {
    let (sid, name, slot) = entry;
    slot.published().map(|frame| StreamFrame { source_id: sid.clone(), name: name.clone(), frame })
}

impl FrameStore for Frames {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        self.streams().iter().filter_map(stream_frame).collect()
    }

    fn stream_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        let sid = source_id.unwrap_or("");
        self.streams().iter().find(|e| e.0 == sid).and_then(stream_frame)
    }

    fn replace_frame(&self, source_id: Option<&str>, frame: Frame, number: u64) -> bool {
        let sid = source_id.unwrap_or("");
        self.streams().iter().find(|e| e.0 == sid).is_some_and(|e| e.2.replace(frame, number))
    }

    fn raw_still(&self, source_id: Option<&str>) -> Option<Frame> {
        let sid = source_id.unwrap_or("");
        self.streams().iter().find(|e| e.0 == sid).and_then(|e| e.2.raw_still())
    }
}

impl StreamFrames for Frames {
    fn new_slot(&self) -> Arc<dyn FrameSlot> {
        Slot::new()
    }

    fn set_streams(&self, streams: Vec<(String, String, Arc<dyn FrameSlot>)>) {
        *self.streams.write().unwrap() = Arc::new(streams);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use telescope_core::frame::PixelFormat;

    fn frame(value: u8) -> Frame {
        Frame::new(4, 2, PixelFormat::I420, vec![value; PixelFormat::I420.size(4, 2)])
    }

    fn streams(list: &[(&str, &str, &Arc<Slot>)]) -> Vec<(String, String, Arc<dyn FrameSlot>)> {
        list.iter().map(|(s, n, slot)| (s.to_string(), n.to_string(), (*slot).clone() as Arc<dyn FrameSlot>)).collect()
    }

    fn rgba(w: u32, h: u32, value: u8) -> Frame {
        Frame::new(w, h, PixelFormat::Rgba, vec![value; PixelFormat::Rgba.size(w, h)])
    }

    #[test]
    fn a_stream_keeps_no_small_copy_until_somebody_asks_and_stops_a_moment_after() {
        let slot = Slot::new();
        assert!(!slot.wants_raw(), "nobody asked");
        assert!(slot.raw_still().is_none(), "the first ask has nothing yet, and starts it");
        assert!(slot.wants_raw());
        slot.offer_raw(&rgba(64, 36, 90));
        assert!(!slot.wants_raw(), "one is made about every 190 ms, not for every frame");
        let still = slot.raw_still().expect("the copy");
        assert_eq!((still.width, still.height, still.format), (64, 36, PixelFormat::Rgba));
        std::thread::sleep(Duration::from_millis(200));
        assert!(slot.wants_raw(), "time for the next while somebody keeps asking");
        // Nobody asked for a while: it stops (the stamp of the last ask moved back past the keep time)
        slot.raw.asked.store(1, Ordering::Relaxed);
        assert!(!slot.wants_raw());
    }

    #[test]
    fn the_small_copy_is_the_frame_before_the_view_shrunk_to_320_wide_keeping_its_shape() {
        let slot = Slot::new();
        slot.raw_still();
        slot.offer_raw(&rgba(1920, 1080, 77));
        let still = slot.raw_still().unwrap();
        assert_eq!((still.width, still.height), (320, 180));
        assert_eq!(&still.data[..4], &[77, 77, 77, 255]);
        // a frame already smaller stays as it is, and a portrait one keeps its shape
        slot.offer_raw(&rgba(100, 50, 5));
        assert_eq!(slot.raw_still().map(|f| (f.width, f.height)), Some((100, 50)));
        slot.offer_raw(&rgba(1080, 1920, 5));
        assert_eq!(slot.raw_still().map(|f| (f.width, f.height)), Some((320, 569)));
    }

    #[test]
    fn the_frame_store_hands_out_a_streams_copy_by_its_source() {
        let (a, b) = (Slot::new(), Slot::new());
        let frames = Frames::default();
        frames.set_streams(streams(&[("a", "A", &a), ("b", "B", &b)]));
        assert!(frames.raw_still(Some("a")).is_none());
        a.offer_raw(&rgba(32, 18, 9));
        assert!(frames.raw_still(Some("a")).is_some());
        assert!(frames.raw_still(Some("b")).is_none(), "the other stream was asked for nothing");
        assert!(frames.raw_still(Some("nobody")).is_none());
    }

    #[test]
    fn a_published_frame_carries_a_number_and_when_it_was_made() {
        let (a, b) = (Slot::new(), Slot::new());
        let before = monotonic_ns();
        a.publish(frame(1));
        b.publish(frame(2));
        a.publish(frame(3));
        let frames = Frames::default();
        frames.set_streams(streams(&[("a", "Phone A", &a), ("b", "Phone B", &b)]));
        let got = frames.stream_frames();
        let (fa, fb) = (&got[0], &got[1]);
        assert_eq!((fa.source_id.as_str(), fa.name.as_str(), fb.source_id.as_str()), ("a", "Phone A", "b"));
        // Numbered across every stream, in the order they came
        assert!(fa.frame.number > fb.frame.number);
        assert!(fa.frame.data[0] == 3 && before <= fa.frame.time_ns && fa.frame.time_ns <= monotonic_ns());
    }

    #[test]
    fn frames_are_handed_out_read_only_and_without_copying() {
        let slot = Slot::new();
        slot.publish(frame(7));
        let frames = Frames::default();
        frames.set_streams(streams(&[("a", "A", &slot)]));
        let mut shared = frames.stream_frame(Some("a")).unwrap();
        assert!(Arc::ptr_eq(&shared.frame.data, &slot.published().unwrap().data));
        // Yours to change, and the stream's is left alone
        shared.frame.data_mut()[0] = 1;
        assert_eq!(slot.published().unwrap().data[0], 7);
    }

    #[test]
    fn streams_without_a_picture_or_that_stopped_are_left_out() {
        let (sending, starting) = (Slot::new(), Slot::new());
        sending.publish(frame(1));
        let frames = Frames::default();
        frames.set_streams(streams(&[("a", "A", &sending), ("b", "B", &starting)]));
        assert_eq!(frames.stream_frames().iter().map(|f| f.source_id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert!(frames.stream_frame(Some("b")).is_none() && frames.stream_frame(Some("nope")).is_none());
        frames.set_streams(streams(&[("b", "B", &starting)]));
        assert!(frames.stream_frames().is_empty() && frames.stream_frame(Some("a")).is_none());
    }

    #[test]
    fn a_dropped_stream_keeps_its_last_frame_which_only_gets_older() {
        let slot = Slot::new();
        slot.publish(frame(1));
        let frames = Frames::default();
        frames.set_streams(streams(&[("a", "A", &slot)]));
        let seen = frames.stream_frame(Some("a")).unwrap();
        assert_eq!(seen.age(seen.frame.time_ns + 5_000_000_000), 5_000_000_000);
        assert_eq!(frames.stream_frame(Some("a")).unwrap().frame.number, seen.frame.number);
        assert_eq!(frames.stream_frame(Some("a")).unwrap().frame.time_ns, seen.frame.time_ns, "reading it again doesn't freshen it");
    }

    #[test]
    fn a_replaced_frame_goes_out_until_a_newer_one_comes() {
        let slot = Slot::new();
        let frames = Frames::default();
        frames.set_streams(streams(&[("", "Main", &slot)]));
        assert!(!frames.replace_frame(None, frame(9), 1));
        let n = slot.publish(frame(1));
        let (_, version) = slot.latest().unwrap();
        assert!(!frames.replace_frame(None, frame(9), n + 1));
        assert!(frames.replace_frame(None, frame(9), n));
        assert!(slot.wait_newer(version, Duration::ZERO));
        assert_eq!(slot.latest().unwrap().0.data[0], 9);
        assert_eq!(slot.latest().unwrap().0.number, n, "the replacement keeps the number it replaced, so it isn't counted as a new frame");
        // The stream's own frame stays what plugins read
        assert_eq!(slot.published().unwrap().data[0], 1);
        slot.publish(frame(2));
        assert!(!frames.replace_frame(None, frame(9), n));
    }

    #[test]
    fn waiting_wakes_on_a_new_frame_from_another_thread() {
        let slot = Slot::new();
        let s = slot.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            s.publish(frame(1));
        });
        assert!(slot.wait_newer(0, Duration::from_secs(5)));
        t.join().unwrap();
        assert!(!slot.wait_newer(1, Duration::from_millis(10)));
    }

    #[test]
    fn reading_while_streams_publish_and_come_and_go_never_sees_a_torn_frame() {
        let slots: Vec<Arc<Slot>> = (0..3).map(|_| Slot::new()).collect();
        let frames = Frames::new();
        let all = |n: usize| slots[..n].iter().enumerate().map(|(i, s)| (i.to_string(), i.to_string(), s.clone() as Arc<dyn FrameSlot>)).collect::<Vec<_>>();
        frames.set_streams(all(3));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        for (i, slot) in slots.iter().enumerate() {
            let (slot, stop) = (slot.clone(), stop.clone());
            threads.push(std::thread::spawn(move || {
                let mut seq = 0u32;
                while !stop.load(Ordering::SeqCst) {
                    seq += 1;
                    slot.publish(Frame::new(8, 8, PixelFormat::I420, vec![(i as u32 * 50 + seq) as u8; PixelFormat::I420.size(8, 8)]));
                }
                Vec::new()
            }));
        }
        for _ in 0..2 {
            let (frames, stop) = (frames.clone(), stop.clone());
            threads.push(std::thread::spawn(move || {
                let mut problems = Vec::new();
                let mut numbers = std::collections::HashMap::new();
                let mut seen = std::collections::BTreeSet::new();
                while !stop.load(Ordering::SeqCst) {
                    for sf in frames.stream_frames() {
                        if !sf.frame.data.iter().all(|&v| v == sf.frame.data[0]) {
                            problems.push("a frame changed while it was being read");
                        }
                        let last = numbers.entry(sf.source_id.clone()).or_insert(0);
                        if sf.frame.number < *last {
                            problems.push("a stream's frame number went backwards");
                        }
                        *last = sf.frame.number;
                        seen.insert(sf.source_id.clone());
                    }
                }
                if seen.into_iter().eq(["0", "1", "2"]) {
                    problems
                } else {
                    vec!["a reader missed a stream"]
                }
            }));
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut flip = 0;
        while Instant::now() < deadline {
            flip += 1;
            frames.set_streams(all(if flip % 2 == 1 { 3 } else { 1 }));
            std::thread::sleep(Duration::from_millis(1)); // a reader gets a look at each, however its thread is scheduled
        }
        stop.store(true, Ordering::SeqCst);
        let problems: Vec<_> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
        assert!(problems.is_empty(), "{problems:?}");
    }
}
