//! A PipeWire video source for pictures Telescope makes: one fixed-size BGRA format, plain shared-memory buffers the stream allocates, and a driving stream that runs a cycle
//! each time a picture is handed over (what a producer does when it has a frame). Programs that read PipeWire video, OBS among them, list it as a camera.

use super::ffi::{PwBuffer, SPA_DATA_MEM_FD};
use super::pod::{self, Choice, ObjectPod, Pod, Prop, Scalar, Value};
use super::{Connection, Stream, StreamConfig, StreamHandler, StreamRef, StreamState, VideoFormat, PW_DIRECTION_OUTPUT, PW_STREAM_FLAG_DRIVER, SPA_ID_INVALID};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 4;

/// The picture the next cycle puts in a buffer, and the size every one has.
struct Latest {
    streaming: AtomicBool,
    /// Why the stream is in an error state, until it leaves it.
    error: Mutex<Option<String>>,
    width: AtomicU32,
    height: AtomicU32,
    /// The size the stream has settled on with whoever reads it, 0 x 0 while it renegotiates after a resize.
    ready_width: AtomicU32,
    ready_height: AtomicU32,
    /// Shared so a cycle copies it outside the lock, and the sender can swap in the next picture meanwhile.
    bytes: Mutex<Arc<Vec<u8>>>,
}

impl Latest {
    fn on_state(&self, state: StreamState, error: Option<String>) {
        self.streaming.store(state == StreamState::Streaming, Ordering::SeqCst);
        let why = (state == StreamState::Error).then(|| error.unwrap_or_else(|| "unknown error".into()));
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = why;
    }

    fn failure(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn size(&self) -> (u32, u32) {
        (self.width.load(Ordering::SeqCst), self.height.load(Ordering::SeqCst))
    }

    fn settled(&self) -> bool {
        (self.ready_width.load(Ordering::SeqCst), self.ready_height.load(Ordering::SeqCst)) == self.size()
    }
}

fn enum_format(width: u32, height: u32) -> Pod {
    let props = [
        Prop { key: pod::FORMAT_MEDIA_TYPE, flags: 0, value: Value::One(Scalar::Id(pod::MEDIA_TYPE_VIDEO)) },
        Prop { key: pod::FORMAT_MEDIA_SUBTYPE, flags: 0, value: Value::One(Scalar::Id(pod::MEDIA_SUBTYPE_RAW)) },
        Prop { key: pod::FORMAT_VIDEO_SIZE, flags: 0, value: Value::One(Scalar::Rect(width, height)) },
        Prop { key: pod::FORMAT_VIDEO_FRAMERATE, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Frac(30, 1), Scalar::Frac(1, 1), Scalar::Frac(1000, 1)]) },
        Prop { key: pod::FORMAT_VIDEO_FORMAT, flags: 0, value: Value::One(Scalar::Id(VideoFormat::Bgra.spa_id())) },
    ];
    Pod::object(pod::OBJECT_FORMAT, pod::PARAM_ENUM_FORMAT, &props)
}

fn buffers(width: u32, height: u32) -> Pod {
    let int = |v: i32| Value::One(Scalar::Int(v));
    let props = [
        Prop { key: pod::BUFFERS_BUFFERS, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Int(4), Scalar::Int(2), Scalar::Int(8)]) },
        Prop { key: pod::BUFFERS_BLOCKS, flags: 0, value: int(1) },
        Prop { key: pod::BUFFERS_DATA_TYPE, flags: 0, value: Value::Choice(Choice::Flags, vec![Scalar::Int(1 << SPA_DATA_MEM_FD)]) },
        Prop { key: pod::BUFFERS_SIZE, flags: 0, value: int((width * height * 4) as i32) },
        Prop { key: pod::BUFFERS_STRIDE, flags: 0, value: int((width * 4) as i32) },
        Prop { key: pod::BUFFERS_ALIGN, flags: 0, value: int(16) },
    ];
    Pod::object(pod::OBJECT_PARAM_BUFFERS, pod::PARAM_BUFFERS, &props)
}

struct Producer(Arc<Latest>);

impl StreamHandler for Producer {
    fn state_changed(&mut self, _stream: &StreamRef, state: StreamState, error: Option<String>) {
        self.0.on_state(state, error);
    }

    fn param_changed(&mut self, stream: &StreamRef, id: u32, format: Option<ObjectPod>) {
        if id != pod::PARAM_FORMAT {
            return;
        }
        if let Some(format) = format {
            // The size this format was agreed for, which a resize since may already have left behind
            let (w, h) = format_size(&format).unwrap_or_else(|| self.0.size());
            stream.update_params(&[buffers(w, h)]);
            self.0.ready_width.store(w, Ordering::SeqCst);
            self.0.ready_height.store(h, Ordering::SeqCst);
        } else {
            self.0.ready_width.store(0, Ordering::SeqCst);
            self.0.ready_height.store(0, Ordering::SeqCst);
        }
    }

    fn process(&mut self, stream: &StreamRef) {
        // Between a resize and the reader settling on the new size, a buffer would be filled for a size it doesn't have yet
        if !self.0.settled() {
            return;
        }
        // A picture of another size, or none yet after a resize, would leave a short or mismatched buffer
        let bytes = self.0.bytes.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (width, height) = self.0.size();
        if bytes.len() != width as usize * height as usize * 4 {
            return;
        }
        let buffer = stream.dequeue();
        if buffer.is_null() {
            return;
        }
        fill(buffer, &self.0, &bytes);
        // SAFETY: the buffer was dequeued from this stream and is queued once here
        unsafe { stream.queue(buffer) };
    }

    fn add_buffer(&mut self, _stream: &StreamRef, _buffer: *mut PwBuffer) {}
}

/// The size a negotiated format carries.
fn format_size(format: &ObjectPod) -> Option<(u32, u32)> {
    match &format.prop(pod::FORMAT_VIDEO_SIZE)?.value {
        Value::One(Scalar::Rect(w, h)) => Some((*w, *h)),
        _ => None,
    }
}

/// Writes the latest picture into a buffer's memory, as much of it as the buffer holds.
fn fill(buffer: *mut PwBuffer, latest: &Latest, bytes: &[u8]) {
    let (width, height) = latest.size();
    let stride = width as usize * 4;
    let wanted = stride * height as usize;
    // SAFETY: the buffer is the stream's own with one mapped data (MAP_BUFFERS), valid until it is queued
    unsafe {
        let data = &mut *(*(*buffer).buffer).datas;
        if data.data.is_null() {
            return;
        }
        let room = (data.maxsize as usize).min(wanted);
        let n = room.min(bytes.len());
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.data.cast::<u8>(), n);
        let chunk = &mut *data.chunk;
        chunk.offset = 0;
        chunk.size = n as u32;
        chunk.stride = stride as i32;
    }
}

/// A video source in PipeWire's graph, gone when dropped.
pub struct VideoSender {
    connection: Arc<Connection>,
    stream: Stream,
    latest: Arc<Latest>,
}

impl VideoSender {
    /// A source called `description` (its node is "telescope-" and the sanitised name) for BGRA pictures of this size.
    pub fn open(connection: &Arc<Connection>, description: &str, width: u32, height: u32) -> Result<VideoSender, String> {
        let readable: String = description.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        // Names that differ only in letters a node name can't hold (accents, other scripts) still get different nodes
        let hash = description.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        let node = format!("telescope-{readable}-{:08x}", hash >> 32);
        let props = [("media.class", "Video/Source"), ("node.name", node.as_str()), ("node.description", description), ("media.role", "Camera")];
        let latest = Arc::new(Latest { streaming: AtomicBool::new(false), error: Mutex::new(None), width: AtomicU32::new(width), height: AtomicU32::new(height), ready_width: AtomicU32::new(0), ready_height: AtomicU32::new(0), bytes: Mutex::new(Arc::default()) });
        let config = StreamConfig { name: description, props: &props, direction: PW_DIRECTION_OUTPUT, target: SPA_ID_INVALID, flags: PW_STREAM_FLAG_DRIVER | PW_STREAM_FLAG_MAP_BUFFERS };
        let stream = Stream::connect(connection, &config, &[enum_format(width, height)], Box::new(Producer(latest.clone())))?;
        Ok(VideoSender { connection: connection.clone(), stream, latest })
    }

    /// Whether a reader is ready for pictures, or a send needs to report a lost connection or a failed stream.
    pub fn wants_frame(&self) -> bool {
        self.latest.streaming.load(Ordering::SeqCst) || self.connection.is_dead() || self.latest.failure().is_some()
    }

    /// Offers the same source at another size, which whoever reads it renegotiates, so the node and its id stay.
    pub fn resize(&self, width: u32, height: u32) {
        // All under the loop lock, so a cycle never sees the new size with the old format or picture
        self.stream.locked(|stream| {
            let latest = &self.latest;
            if latest.size() != (width, height) {
                latest.ready_width.store(0, Ordering::SeqCst);
                latest.ready_height.store(0, Ordering::SeqCst);
            }
            latest.width.store(width, Ordering::SeqCst);
            latest.height.store(height, Ordering::SeqCst);
            *latest.bytes.lock().unwrap_or_else(|e| e.into_inner()) = Arc::default();
            stream.update_params(&[enum_format(width, height)]);
        });
    }

    /// The next picture, `width * height * 4` bytes: it goes out on the cycle this starts.
    pub fn send(&self, bgra: &[u8]) -> Result<(), String> {
        self.check(bgra.len())?;
        self.put(bgra.to_vec());
        Ok(())
    }

    /// send, taking the picture's buffer; what comes back in its place is the buffer of an earlier picture no cycle is still copying, or an empty one.
    pub fn send_buffer(&self, bgra: &mut Vec<u8>) -> Result<(), String> {
        self.check(bgra.len())?;
        let old = self.put(std::mem::take(bgra));
        *bgra = Arc::try_unwrap(old).unwrap_or_default();
        Ok(())
    }

    fn check(&self, len: usize) -> Result<(), String> {
        if self.connection.is_dead() {
            return Err("PipeWire went away".into());
        }
        if let Some(why) = self.latest.failure() {
            return Err(format!("the PipeWire stream failed: {why}"));
        }
        let (width, height) = self.latest.size();
        let wanted = width as usize * height as usize * 4;
        if len != wanted {
            return Err(format!("a picture of {len} bytes for a sender of {wanted}"));
        }
        Ok(())
    }

    fn put(&self, bgra: Vec<u8>) -> Arc<Vec<u8>> {
        let old = std::mem::replace(&mut *self.latest.bytes.lock().unwrap_or_else(|e| e.into_inner()), Arc::new(bgra));
        self.stream.trigger_process();
        old
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest() -> Latest {
        Latest { streaming: AtomicBool::new(false), error: Mutex::new(None), width: AtomicU32::new(4), height: AtomicU32::new(4), ready_width: AtomicU32::new(0), ready_height: AtomicU32::new(0), bytes: Mutex::new(Arc::default()) }
    }

    #[test]
    fn a_stream_error_is_kept_until_the_stream_leaves_it() {
        let l = latest();
        l.on_state(StreamState::Streaming, None);
        assert_eq!(l.failure(), None);
        l.on_state(StreamState::Error, Some("no memory".into()));
        assert_eq!(l.failure().as_deref(), Some("no memory"));
        assert!(!l.streaming.load(Ordering::SeqCst));
        l.on_state(StreamState::Error, None);
        assert!(l.failure().is_some());
        l.on_state(StreamState::Paused, None);
        assert_eq!(l.failure(), None);
    }

    #[test]
    fn a_negotiated_format_says_which_size_it_was_agreed_for() {
        let format = ObjectPod::parse(&enum_format(1280, 720).bytes()).unwrap();
        assert_eq!(format_size(&format), Some((1280, 720)));
    }
}
