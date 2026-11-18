//! A PipeWire video source for pictures Telescope makes: one fixed-size BGRA format, plain shared-memory buffers the stream allocates, and a driving stream that runs a cycle
//! each time a picture is handed over (what a producer does when it has a frame). Programs that read PipeWire video, OBS among them, list it as a camera.

use super::ffi::{PwBuffer, SPA_DATA_MEM_FD};
use super::pod::{self, Choice, ObjectPod, Pod, Prop, Scalar, Value};
use super::{Connection, Stream, StreamConfig, StreamHandler, StreamRef, VideoFormat, PW_DIRECTION_OUTPUT, PW_STREAM_FLAG_DRIVER, SPA_ID_INVALID};
use std::sync::{Arc, Mutex};

const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 4;

/// The picture the next cycle puts in a buffer, and the size every one has.
struct Latest {
    width: u32,
    height: u32,
    bytes: Mutex<Vec<u8>>,
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
    fn param_changed(&mut self, stream: &StreamRef, id: u32, format: Option<ObjectPod>) {
        if id == pod::PARAM_FORMAT && format.is_some() {
            stream.update_params(&[buffers(self.0.width, self.0.height)]);
        }
    }

    fn process(&mut self, stream: &StreamRef) {
        let buffer = stream.dequeue();
        if buffer.is_null() {
            return;
        }
        fill(buffer, &self.0);
        // SAFETY: the buffer was dequeued from this stream and is queued once here
        unsafe { stream.queue(buffer) };
    }

    fn add_buffer(&mut self, _stream: &StreamRef, _buffer: *mut PwBuffer) {}
}

/// Writes the latest picture into a buffer's memory, as much of it as the buffer holds.
fn fill(buffer: *mut PwBuffer, latest: &Latest) {
    let stride = latest.width as usize * 4;
    let wanted = stride * latest.height as usize;
    // SAFETY: the buffer is the stream's own with one mapped data (MAP_BUFFERS), valid until it is queued
    unsafe {
        let data = &mut *(*(*buffer).buffer).datas;
        if data.data.is_null() {
            return;
        }
        let room = (data.maxsize as usize).min(wanted);
        let bytes = latest.bytes.lock().unwrap_or_else(|e| e.into_inner());
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
    stream: Stream,
    latest: Arc<Latest>,
}

impl VideoSender {
    /// A source called `description` (its node is "telescope-" and the sanitised name) for BGRA pictures of this size.
    pub fn open(connection: &Arc<Connection>, description: &str, width: u32, height: u32) -> Result<VideoSender, String> {
        let node: String = description.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        let node = format!("telescope-{node}");
        let props = [("media.class", "Video/Source"), ("node.name", node.as_str()), ("node.description", description), ("media.role", "Camera")];
        let latest = Arc::new(Latest { width, height, bytes: Mutex::new(Vec::new()) });
        let config = StreamConfig { name: description, props: &props, direction: PW_DIRECTION_OUTPUT, target: SPA_ID_INVALID, flags: PW_STREAM_FLAG_DRIVER | PW_STREAM_FLAG_MAP_BUFFERS };
        let stream = Stream::connect(connection, &config, &[enum_format(width, height)], Box::new(Producer(latest.clone())))?;
        Ok(VideoSender { stream, latest })
    }

    /// The next picture, `width * height * 4` bytes: it goes out on the cycle this starts.
    pub fn send(&self, bgra: &[u8]) -> Result<(), String> {
        let wanted = self.latest.width as usize * self.latest.height as usize * 4;
        if bgra.len() != wanted {
            return Err(format!("a picture of {} bytes for a sender of {wanted}", bgra.len()));
        }
        {
            let mut bytes = self.latest.bytes.lock().unwrap_or_else(|e| e.into_inner());
            bytes.clear();
            bytes.extend_from_slice(bgra);
        }
        self.stream.trigger_process();
        Ok(())
    }
}
