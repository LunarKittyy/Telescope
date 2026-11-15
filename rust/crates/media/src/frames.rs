//! Frames from other streams (HostFrames): the newest picture of every running stream, for plugins that combine them.
//! The window says which streams run and plugins ask from any thread, often from inside another stream's frame steps.
//! A stream swaps in its newest frame whole and never changes it after, so handing one out shares it: no copy.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};
use telescope_core::clock::{ClockSync, PhoneClock};
use telescope_core::frame::{Frame, StreamFrame};
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
    /// How many of the newest published frames are kept, shared by every slot of one Frames: 0 keeps none.
    history: Arc<AtomicUsize>,
    /// The pipeline epoch shared by every slot of one Frames (FrameStore::pipeline_epoch): a past frame made under an older one isn't handed out.
    epoch: Arc<AtomicU64>,
    /// What this stream's capture times were read against.
    clock: Mutex<Option<Arc<PhoneClock>>>,
}

#[derive(Default)]
struct SlotState {
    published: Option<Frame>,
    latest: Option<Frame>,
    version: u64,
    /// The last few published frames and the epoch each was published under, oldest first, held only while something asked for history (they're shared, not copies).
    past: VecDeque<(Frame, u64)>,
}

impl Slot {
    pub fn new() -> Arc<Slot> {
        Arc::new(Slot::default())
    }

    /// A slot that keeps as many past frames as `history` says, now and later.
    pub fn with_history(history: Arc<AtomicUsize>) -> Arc<Slot> {
        Arc::new(Slot { history, ..Slot::default() })
    }

    /// A slot that also shares the pipeline epoch of the Frames it belongs to.
    fn with_shared(history: Arc<AtomicUsize>, epoch: Arc<AtomicU64>) -> Arc<Slot> {
        Arc::new(Slot { history, epoch, ..Slot::default() })
    }

    /// Lets go of past frames beyond what history allows now, so asking for fewer (or none) frees them without waiting for the next frame.
    fn trim_history(&self) {
        let keep = self.history.load(Ordering::Relaxed);
        let mut st = self.state.lock().unwrap();
        while st.past.len() > keep {
            st.past.pop_front();
        }
    }

    /// How many past frames it holds right now.
    pub fn held(&self) -> usize {
        self.state.lock().unwrap().past.len()
    }
}

impl Slot {
    /// Numbers and keeps a frame; `entered` is the epoch its steps began under, None for the epoch now.
    fn put_out(&self, mut frame: Frame, entered: Option<u64>) -> u64 {
        let mut st = self.state.lock().unwrap();
        // Numbered under the lock, so a stream's numbers only go up in the order its frames come out
        let number = FRAME_NUMBERS.fetch_add(1, Ordering::SeqCst);
        frame.number = number;
        frame.time_ns = monotonic_ns();
        let keep = self.history.load(Ordering::Relaxed);
        if keep == 0 {
            st.past.clear();
        } else {
            // read under the lock, so a bump that comes after is told from this frame
            st.past.push_back((frame.clone(), entered.unwrap_or_else(|| self.epoch.load(Ordering::SeqCst))));
            while st.past.len() > keep {
                st.past.pop_front();
            }
        }
        st.published = Some(frame.clone());
        st.latest = Some(frame);
        st.version += 1;
        self.changed.notify_all();
        number
    }

    /// `replace`, and when `expected` is given only while the camera's frame still has those pixels, checked under the same lock.
    fn replace_when(&self, mut frame: Frame, number: u64, expected: Option<&Arc<Vec<u8>>>) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.latest.is_none() || st.published.as_ref().map(|f| f.number) != Some(number) {
            return false;
        }
        if expected.is_some_and(|e| st.latest.as_ref().is_none_or(|f| !Arc::ptr_eq(&f.data, e))) {
            return false;
        }
        // Still the frame that went out under this number, so a stream counting what it sends doesn't take it for a new one
        frame.number = number;
        st.latest = Some(frame);
        st.version += 1;
        self.changed.notify_all();
        true
    }
}

impl FrameSlot for Slot {
    fn publish(&self, frame: Frame) -> u64 {
        self.put_out(frame, None)
    }

    fn publish_at(&self, frame: Frame, epoch: u64) -> u64 {
        self.put_out(frame, Some(epoch))
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn published(&self) -> Option<Frame> {
        self.state.lock().unwrap().published.clone()
    }

    fn replace(&self, frame: Frame, number: u64) -> bool {
        self.replace_when(frame, number, None)
    }

    fn replace_if(&self, frame: Frame, number: u64, expected: &Arc<Vec<u8>>) -> bool {
        self.replace_when(frame, number, Some(expected))
    }

    fn latest(&self) -> Option<(Frame, u64)> {
        let st = self.state.lock().unwrap();
        st.latest.clone().map(|f| (f, st.version))
    }

    fn set_phone_clock(&self, clock: Option<Arc<PhoneClock>>) {
        *self.clock.lock().unwrap() = clock;
    }

    /// The phone clock, only while the stream's newest frame carries a capture time: a stream without the phone's stamps (MJPEG, H.264 with no
    /// SEI) has no capture times to be off by anything.
    fn phone_clock(&self) -> Option<Arc<PhoneClock>> {
        let stamped = self.state.lock().unwrap().published.as_ref().is_some_and(|f| f.capture_ns.is_some());
        stamped.then(|| self.clock.lock().unwrap().clone()).flatten()
    }

    fn forget_history(&self) {
        let mut st = self.state.lock().unwrap();
        // bumped before the frames go, so a lookup that races this one never sees an older one pass as current
        self.epoch.fetch_add(1, Ordering::SeqCst);
        st.past.clear();
    }

    fn near(&self, capture_ns: u64) -> Option<Frame> {
        let st = self.state.lock().unwrap();
        let epoch = self.epoch.load(Ordering::SeqCst);
        // Newest first, so of two as near the newer one is the first and wins
        st.past.iter().rev().filter(|(_, e)| *e >= epoch).filter_map(|(f, _)| f.capture_ns.map(|c| (c.abs_diff(capture_ns), f))).min_by_key(|(gap, _)| *gap).map(|(_, f)| f.clone())
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
    /// How many past frames each stream's slot keeps (StreamFrames::set_history).
    history: Arc<AtomicUsize>,
    /// The slots made so far, to trim when history shrinks.
    slots: Mutex<Vec<Weak<Slot>>>,
    /// Goes up whenever any stream's steps are handed out again (Slot::forget_history), for all of them at once.
    epoch: Arc<AtomicU64>,
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

    fn replace_frame_if(&self, source_id: Option<&str>, expected: &Arc<Vec<u8>>, frame: Frame, number: u64) -> bool {
        let sid = source_id.unwrap_or("");
        self.streams().iter().find(|e| e.0 == sid).is_some_and(|e| e.2.replace_if(frame, number, expected))
    }

    fn stream_frame_near(&self, source_id: Option<&str>, capture_ns: u64) -> Option<StreamFrame> {
        let sid = source_id.unwrap_or("");
        let streams = self.streams();
        let (sid, name, slot) = streams.iter().find(|e| e.0 == sid)?;
        slot.near(capture_ns).map(|frame| StreamFrame { source_id: sid.clone(), name: name.clone(), frame })
    }

    fn outgoing_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        let sid = source_id.unwrap_or("");
        let streams = self.streams();
        let (sid, name, slot) = streams.iter().find(|e| e.0 == sid)?;
        slot.latest().map(|(frame, _)| StreamFrame { source_id: sid.clone(), name: name.clone(), frame })
    }

    fn pipeline_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn clock_sync(&self, source_id: Option<&str>) -> Option<ClockSync> {
        let sid = source_id.unwrap_or("");
        self.streams().iter().find(|e| e.0 == sid).and_then(|e| e.2.phone_clock()).and_then(|c| c.sync())
    }
}

impl StreamFrames for Frames {
    fn new_slot(&self) -> Arc<dyn FrameSlot> {
        let slot = Slot::with_shared(self.history.clone(), self.epoch.clone());
        let mut slots = self.slots.lock().unwrap();
        slots.retain(|w| w.strong_count() > 0);
        slots.push(Arc::downgrade(&slot));
        slot
    }

    fn set_history(&self, frames: usize) {
        self.history.store(frames, Ordering::Relaxed);
        let slots: Vec<_> = self.slots.lock().unwrap().iter().filter_map(Weak::upgrade).collect();
        for slot in slots {
            slot.trim_history();
        }
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
    fn forgetting_history_leaves_nothing_for_near_to_hand_back() {
        let slot = Slot::with_history(Arc::new(AtomicUsize::new(4)));
        for ms in [100u64, 133] {
            let mut f = frame(1);
            f.capture_ns = Some(ms * 1_000_000);
            slot.publish(f);
        }
        assert_eq!(slot.near(100_000_000).and_then(|f| f.capture_ns), Some(100_000_000));
        slot.forget_history();
        assert!(slot.near(100_000_000).is_none(), "a frame from before the steps changed isn't handed out");
        assert!(slot.published().is_some(), "the newest frame is still the stream's");
    }

    #[test]
    fn a_frame_is_told_by_the_epoch_it_was_made_under_not_the_one_it_comes_out_in() {
        let frames = Frames::new();
        frames.set_history(4);
        let (a, b) = (frames.new_slot(), frames.new_slot());
        frames.set_streams(vec![("a".into(), "A".into(), a.clone()), ("b".into(), "B".into(), b.clone())]);
        // a's steps began on this frame, then b's steps were handed out, then a's frame came out
        let entered = a.epoch();
        b.forget_history();
        a.publish_at(taken_at(1, Some(100)), entered);
        assert!(frames.stream_frame_near(Some("a"), 100).is_none(), "made before the change, so it isn't handed out");
        a.publish_at(taken_at(2, Some(133)), a.epoch());
        assert_eq!(frames.stream_frame_near(Some("a"), 100).map(|f| f.frame.data[0]), Some(2));
    }

    #[test]
    fn replace_if_only_replaces_the_picture_it_was_told_about() {
        let slot = Slot::new();
        let number = slot.publish(frame(1));
        let (shown, _) = slot.latest().unwrap();
        // another plugin put its own picture out in place of it
        assert!(slot.replace(frame(2), number));
        assert!(!slot.replace_if(frame(3), number, &shown.data), "it isn't the one that was meant");
        assert_eq!(slot.latest().unwrap().0.data[0], 2);
        let (now, _) = slot.latest().unwrap();
        assert!(slot.replace_if(frame(4), number, &now.data));
        assert_eq!(slot.latest().unwrap().0.data[0], 4);
    }

    #[test]
    fn a_change_of_steps_on_any_stream_hides_every_streams_older_history() {
        let frames = Frames::new();
        frames.set_history(4);
        let (a, b) = (frames.new_slot(), frames.new_slot());
        frames.set_streams(vec![("a".into(), "A".into(), a.clone()), ("b".into(), "B".into(), b.clone())]);
        for (slot, base) in [(&a, 100u64), (&b, 200)] {
            slot.publish(taken_at(1, Some(base)));
            slot.publish(taken_at(2, Some(base + 33)));
        }
        let epoch = frames.pipeline_epoch();
        assert_eq!(frames.stream_frame_near(Some("a"), 100).map(|f| f.frame.data[0]), Some(1));
        assert_eq!(frames.stream_frame_near(Some("b"), 200).map(|f| f.frame.data[0]), Some(1));
        // Only B's steps are handed out again, but A may blur too (a flag they share), so A's clear history can't stay
        b.forget_history();
        assert!(frames.pipeline_epoch() > epoch);
        assert!(frames.stream_frame_near(Some("a"), 100).is_none() && frames.stream_frame_near(Some("a"), 133).is_none());
        assert!(frames.stream_frame_near(Some("b"), 200).is_none());
        assert_eq!(frames.stream_frame(Some("a")).unwrap().frame.data[0], 2, "the newest frame is still there to fall back to");
        // Frames published after it are of the new epoch and come back, next to none of the old ones
        a.publish(taken_at(3, Some(166)));
        assert_eq!(frames.stream_frame_near(Some("a"), 100).map(|f| f.frame.data[0]), Some(3), "never one of the older ones, even if they were nearer");
        assert!(frames.stream_frame_near(Some("b"), 200).is_none());
    }

    #[test]
    fn what_goes_out_is_the_replaced_frame_when_there_is_one() {
        let slot = Slot::new();
        let frames = Frames::default();
        frames.set_streams(streams(&[("", "Main", &slot)]));
        assert!(frames.outgoing_frame(None).is_none());
        let n = slot.publish(frame(1));
        assert_eq!(frames.outgoing_frame(None).map(|f| (f.frame.data[0], f.frame.number)), Some((1, n)));
        assert!(frames.replace_frame(None, frame(9), n));
        assert_eq!(frames.outgoing_frame(None).map(|f| (f.frame.data[0], f.frame.number)), Some((9, n)), "the replacement, under the number it replaced");
        assert_eq!(frames.stream_frame(None).unwrap().frame.data[0], 1, "the stream's own frame is what plugins read");
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

    fn taken_at(value: u8, capture_ns: Option<u64>) -> Frame {
        let mut f = frame(value);
        f.capture_ns = capture_ns;
        f
    }

    /// A Frames with one stream "a" in a slot that keeps `history` frames.
    fn one_stream(history: usize) -> (Arc<Frames>, Arc<dyn FrameSlot>) {
        let frames = Frames::new();
        frames.set_history(history);
        let slot = frames.new_slot();
        frames.set_streams(vec![("a".into(), "Phone A".into(), slot.clone())]);
        (frames, slot)
    }

    #[test]
    fn the_capture_time_survives_publishing_and_replacing() {
        let slot = Slot::new();
        let n = slot.publish(taken_at(1, Some(1_234)));
        assert_eq!(slot.published().unwrap().capture_ns, Some(1_234));
        assert_eq!(slot.latest().unwrap().0.capture_ns, Some(1_234));
        // A replacement made from that frame carries its own, as a step that builds a new picture hands on
        assert!(slot.replace(taken_at(2, Some(1_234)), n));
        assert_eq!(slot.latest().unwrap().0.capture_ns, Some(1_234));
        assert_eq!(slot.published().unwrap().capture_ns, Some(1_234));
    }

    #[test]
    fn the_frame_nearest_a_capture_time_comes_from_the_history() {
        let (frames, slot) = one_stream(6);
        for (value, at) in [(1, 1_000), (2, 1_034), (3, 1_066), (4, 1_100)] {
            slot.publish(taken_at(value, Some(at)));
        }
        let near = |at: u64| frames.stream_frame_near(Some("a"), at).map(|f| (f.frame.data[0], f.source_id, f.name));
        assert_eq!(near(1_040), Some((2, "a".into(), "Phone A".into())));
        assert_eq!(near(1_060).map(|f| f.0), Some(3));
        assert_eq!(near(0).map(|f| f.0), Some(1), "before all of them it's the first");
        assert_eq!(near(9_999).map(|f| f.0), Some(4), "after all of them it's the newest");
        // Right between two, the newer
        assert_eq!(near(1_049).map(|f| f.0), Some(2));
        assert_eq!(near(1_050).map(|f| f.0), Some(3));
        assert!(frames.stream_frame_near(Some("nobody"), 1_000).is_none());
    }

    #[test]
    fn it_looks_at_capture_times_not_when_frames_were_published() {
        let (frames, slot) = one_stream(6);
        // Published in this order, taken in another
        slot.publish(taken_at(1, Some(5_000)));
        slot.publish(taken_at(2, Some(1_000)));
        slot.publish(taken_at(3, None));
        assert_eq!(frames.stream_frame_near(Some("a"), 1_100).map(|f| f.frame.data[0]), Some(2));
        assert_eq!(frames.stream_frame_near(Some("a"), 4_000).map(|f| f.frame.data[0]), Some(1));
    }

    #[test]
    fn a_stream_with_no_capture_times_has_nothing_near() {
        let (frames, slot) = one_stream(6);
        slot.publish(taken_at(1, None));
        slot.publish(taken_at(2, None));
        assert!(frames.stream_frame_near(Some("a"), 1_000).is_none());
    }

    #[test]
    fn the_history_holds_as_many_frames_as_asked_and_no_more() {
        let frames = Frames::new();
        frames.set_history(3);
        let slot = Slot::with_history(frames.history.clone());
        for i in 0..10u8 {
            slot.publish(taken_at(i, Some(u64::from(i) * 100)));
        }
        assert_eq!(slot.held(), 3);
        assert_eq!(slot.near(0).unwrap().data[0], 7, "the oldest kept is the 8th");
        // Asking for fewer takes effect with the next frame, and asking for none empties it
        frames.set_history(1);
        slot.publish(taken_at(10, Some(1_000)));
        assert_eq!(slot.held(), 1);
        frames.set_history(0);
        slot.publish(taken_at(11, Some(1_100)));
        assert_eq!(slot.held(), 0);
        assert!(slot.near(1_100).is_none());
    }

    #[test]
    fn asking_for_no_history_frees_what_was_held_at_once() {
        let (frames, slot) = one_stream(4);
        for i in 0..4u8 {
            slot.publish(taken_at(i, Some(u64::from(i))));
        }
        let held = frames.slots.lock().unwrap()[0].upgrade().unwrap();
        assert_eq!(held.held(), 4);
        frames.set_history(0);
        assert_eq!(held.held(), 0, "no publish needed");
    }

    #[test]
    fn no_history_is_kept_when_nobody_asked() {
        let (frames, slot) = one_stream(0);
        let first = taken_at(1, Some(1_000));
        let watch = Arc::downgrade(&first.data);
        slot.publish(first);
        for i in 2..6u8 {
            slot.publish(taken_at(i, Some(u64::from(i) * 1_000)));
        }
        // Only the newest is held: the first one's pixels are gone, and a lookup has nothing to find
        assert!(watch.upgrade().is_none(), "an older frame is still held");
        assert!(frames.stream_frame_near(Some("a"), 1_000).is_none());
        assert_eq!(frames.stream_frame(Some("a")).unwrap().frame.data[0], 5);
    }

    #[test]
    fn the_replaced_picture_for_the_camera_is_not_what_the_history_holds() {
        let (frames, slot) = one_stream(4);
        let n = slot.publish(taken_at(1, Some(1_000)));
        assert!(slot.replace(taken_at(9, Some(1_000)), n));
        assert_eq!(frames.stream_frame_near(Some("a"), 1_000).unwrap().frame.data[0], 1, "the stream's own frame");
    }

    #[test]
    fn a_streams_sync_error_comes_from_its_phone_clock() {
        let (frames, slot) = one_stream(0);
        assert_eq!(frames.clock_sync(Some("a")), None, "no clock yet");
        let clock = Arc::new(PhoneClock::default());
        slot.set_phone_clock(Some(clock.clone()));
        assert_eq!(frames.clock_sync(Some("a")), None, "no round trip yet");
        clock.set(telescope_core::clock::ClockOffset { boot_ns: Some(5), mono_ns: None, error_ns: 1_500_000 });
        assert_eq!(frames.clock_sync(Some("a")), None, "no frame with a capture time yet");
        slot.publish(taken_at(1, None));
        assert_eq!(frames.clock_sync(Some("a")), None, "frames without a capture time");
        slot.publish(taken_at(2, Some(10)));
        assert_eq!(frames.clock_sync(Some("a")), Some(telescope_core::clock::ClockSync { error_ns: 1_500_000 }));
        assert_eq!(frames.clock_sync(Some("b")), None);
        slot.set_phone_clock(None);
        assert_eq!(frames.clock_sync(Some("a")), None, "a stream with no phone clock says nothing");
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
