//! Reading a video node as packed frames: offering the 32-bit RGB formats, taking whichever buffer kind the producer
//! gives (shared memory, or a DMA-BUF it allocated in a layout the CPU can read) and copying the picture out row by row.

use super::connection::Connection;
use super::ffi::{self, PwBuffer, SpaData, StreamState};
use super::pod::{self, Choice, ObjectPod, Pod, Prop, Scalar, Value};
use super::stream::{Stream, StreamConfig, StreamHandler, StreamRef, PW_DIRECTION_INPUT, PW_STREAM_FLAG_AUTOCONNECT};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// DRM_FORMAT_MOD_LINEAR: rows one after another, which is the only DMA-BUF layout the CPU can read.
const MODIFIER_LINEAR: i64 = 0;
/// How often a wait looks whether the node has gone.
const GONE_CHECK: Duration = Duration::from_millis(250);

/// The packed 32-bit pictures frames come as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoFormat {
    Bgra,
    Rgba,
    Bgrx,
    Rgbx,
}

impl VideoFormat {
    /// What gets asked for, in the order of preference.
    pub const ALL: [VideoFormat; 4] = [VideoFormat::Bgra, VideoFormat::Rgba, VideoFormat::Bgrx, VideoFormat::Rgbx];

    /// The name the byte order goes by: "BGRA", "RGBA", "BGRx" or "RGBx".
    pub fn fourcc(self) -> &'static str {
        match self {
            VideoFormat::Bgra => "BGRA",
            VideoFormat::Rgba => "RGBA",
            VideoFormat::Bgrx => "BGRx",
            VideoFormat::Rgbx => "RGBx",
        }
    }

    /// SPA's number for it (spa_video_format).
    pub fn spa_id(self) -> u32 {
        match self {
            VideoFormat::Rgbx => 7,
            VideoFormat::Bgrx => 8,
            VideoFormat::Rgba => 11,
            VideoFormat::Bgra => 12,
        }
    }

    pub fn from_spa_id(id: u32) -> Option<VideoFormat> {
        VideoFormat::ALL.into_iter().find(|f| f.spa_id() == id)
    }
}

/// One picture: `width * height * 4` bytes, rows packed with no padding.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: VideoFormat,
    pub data: Vec<u8>,
}

#[derive(Default)]
struct SlotState {
    frame: Option<VideoFrame>,
    error: Option<String>,
}

#[derive(Default)]
struct Slot {
    state: Mutex<SlotState>,
    arrived: Condvar,
}

impl Slot {
    fn fail(&self, message: String) {
        if let Ok(mut state) = self.state.lock() {
            state.error.get_or_insert(message);
        }
        self.arrived.notify_all();
    }
}

/// What a settled format means for reading buffers.
#[derive(Clone, Copy)]
struct Negotiated {
    width: u32,
    height: u32,
    format: VideoFormat,
}

struct Handler {
    slot: Arc<Slot>,
    negotiated: Option<Negotiated>,
}

fn format_offer(format: VideoFormat, modifiers: bool) -> Pod {
    let mut props = vec![
        Prop { key: pod::FORMAT_MEDIA_TYPE, flags: 0, value: Value::One(Scalar::Id(pod::MEDIA_TYPE_VIDEO)) },
        Prop { key: pod::FORMAT_MEDIA_SUBTYPE, flags: 0, value: Value::One(Scalar::Id(pod::MEDIA_SUBTYPE_RAW)) },
        Prop { key: pod::FORMAT_VIDEO_FORMAT, flags: 0, value: Value::One(Scalar::Id(format.spa_id())) },
    ];
    if modifiers {
        // A producer that fixes the modifier itself (libfunnel does) wants the list not fixed here, and the list is linear only
        props.push(Prop { key: pod::FORMAT_VIDEO_MODIFIER, flags: pod::PROP_MANDATORY | pod::PROP_DONT_FIXATE, value: Value::Choice(Choice::Enum, vec![Scalar::Long(MODIFIER_LINEAR), Scalar::Long(MODIFIER_LINEAR)]) });
    }
    props.push(Prop { key: pod::FORMAT_VIDEO_SIZE, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Rect(1280, 720), Scalar::Rect(1, 1), Scalar::Rect(16384, 16384)]) });
    props.push(Prop { key: pod::FORMAT_VIDEO_FRAMERATE, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Frac(30, 1), Scalar::Frac(0, 1), Scalar::Frac(1000, 1)]) });
    Pod::object(pod::OBJECT_FORMAT, pod::PARAM_ENUM_FORMAT, &props)
}

/// The formats offered: the DMA-BUF kind first, since a producer that can only share DMA-BUFs wants it, then plain memory.
fn format_offers() -> Vec<Pod> {
    [true, false].into_iter().flat_map(|modifiers| VideoFormat::ALL.map(|f| format_offer(f, modifiers))).collect()
}

fn buffers_request(negotiated: Negotiated, dma_buf: bool) -> Pod {
    let int = |v: i32| Value::One(Scalar::Int(v));
    let flags = |mask: u32| Value::Choice(Choice::Flags, vec![Scalar::Int(mask as i32)]);
    let mut props = Vec::new();
    if dma_buf {
        // The producer allocates and says how, so only the kind is asked for
        props.push(Prop { key: pod::BUFFERS_DATA_TYPE, flags: 0, value: flags(1 << ffi::SPA_DATA_DMA_BUF) });
    } else {
        let stride = negotiated.width as i32 * 4;
        props.push(Prop { key: pod::BUFFERS_BUFFERS, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Int(4), Scalar::Int(2), Scalar::Int(16)]) });
        props.push(Prop { key: pod::BUFFERS_BLOCKS, flags: 0, value: int(1) });
        props.push(Prop { key: pod::BUFFERS_SIZE, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Int(stride * negotiated.height as i32), Scalar::Int(stride * negotiated.height as i32), Scalar::Int(i32::MAX)]) });
        props.push(Prop { key: pod::BUFFERS_STRIDE, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Int(stride), Scalar::Int(0), Scalar::Int(i32::MAX)]) });
        props.push(Prop { key: pod::BUFFERS_ALIGN, flags: 0, value: int(16) });
        props.push(Prop { key: pod::BUFFERS_DATA_TYPE, flags: 0, value: flags((1 << ffi::SPA_DATA_MEM_PTR) | (1 << ffi::SPA_DATA_MEM_FD)) });
    }
    Pod::object(pod::OBJECT_PARAM_BUFFERS, pod::PARAM_BUFFERS, &props)
}

impl StreamHandler for Handler {
    fn state_changed(&mut self, _stream: &StreamRef, state: StreamState, error: Option<String>) {
        if state == StreamState::Error {
            self.slot.fail(error.unwrap_or_else(|| "the stream failed".into()));
        }
    }

    fn param_changed(&mut self, stream: &StreamRef, id: u32, format: Option<ObjectPod>) {
        if id != pod::PARAM_FORMAT {
            return;
        }
        let Some(format) = format else {
            self.negotiated = None;
            return;
        };
        let modifier = format.prop(pod::FORMAT_VIDEO_MODIFIER);
        // The producer is still picking a modifier from the list and sends the settled format next
        if modifier.is_some_and(|m| m.flags & pod::PROP_DONT_FIXATE != 0) {
            return;
        }
        let spa_format = format.prop(pod::FORMAT_VIDEO_FORMAT).and_then(|p| p.value.default_value());
        let size = format.prop(pod::FORMAT_VIDEO_SIZE).and_then(|p| p.value.default_value());
        let (Some(Scalar::Id(spa_format)), Some(Scalar::Rect(width, height))) = (spa_format, size) else {
            self.slot.fail("the producer's format has no size".into());
            return;
        };
        let Some(video_format) = VideoFormat::from_spa_id(spa_format) else {
            self.slot.fail(format!("the producer sends a pixel format that isn't read (SPA format {spa_format})"));
            return;
        };
        let dma_buf = modifier.is_some();
        if let Some(Scalar::Long(m)) = modifier.and_then(|m| m.value.default_value()) {
            if m != MODIFIER_LINEAR {
                self.slot.fail(format!("the producer shares pictures in a GPU-only layout (modifier {m:#x}), which can't be read on the CPU"));
                return;
            }
        }
        let negotiated = Negotiated { width, height, format: video_format };
        self.negotiated = Some(negotiated);
        stream.update_params(&[buffers_request(negotiated, dma_buf)]);
    }

    fn process(&mut self, stream: &StreamRef) {
        // The newest buffer is the picture, any older one is given back unread
        let mut newest: *mut PwBuffer = std::ptr::null_mut();
        loop {
            let next = stream.dequeue();
            if next.is_null() {
                break;
            }
            if !newest.is_null() {
                stream.queue(newest);
            }
            newest = next;
        }
        if newest.is_null() {
            return;
        }
        let Some(negotiated) = self.negotiated else {
            stream.queue(newest);
            return;
        };
        // SAFETY: the buffer was dequeued from the stream and stays valid until it is queued again below
        let read = unsafe { read_buffer(newest, negotiated) };
        stream.queue(newest);
        match read {
            Ok(Some(data)) => {
                let frame = VideoFrame { width: negotiated.width, height: negotiated.height, format: negotiated.format, data };
                if let Ok(mut state) = self.slot.state.lock() {
                    state.frame = Some(frame);
                }
                self.slot.arrived.notify_all();
            }
            Ok(None) => {}
            Err(message) => self.slot.fail(message),
        }
    }
}

/// Copies the picture out of a buffer into packed rows: None when the buffer carries no picture this time. Needs a valid dequeued pw_buffer.
unsafe fn read_buffer(buffer: *mut PwBuffer, negotiated: Negotiated) -> Result<Option<Vec<u8>>, String> {
    // SAFETY: a dequeued pw_buffer points at its spa_buffer
    let spa = unsafe { (*buffer).buffer.as_ref() }.ok_or("a buffer has no data")?;
    if spa.n_datas == 0 || spa.datas.is_null() {
        return Ok(None);
    }
    // SAFETY: datas holds n_datas entries
    let data: &SpaData = unsafe { &*spa.datas };
    // SAFETY: the chunk is null or the buffer's own
    let Some(chunk) = (unsafe { data.chunk.as_ref() }) else { return Ok(None) };
    // A chunk flagged corrupted (bit 0) or empty is no picture
    if chunk.size == 0 || chunk.flags & 1 != 0 {
        return Ok(None);
    }
    let row = negotiated.width as usize * 4;
    let stride = if chunk.stride > 0 { chunk.stride as usize } else { row };
    let height = negotiated.height as usize;
    if height == 0 {
        return Ok(None);
    }
    let needed = chunk.offset as usize + stride * (height - 1) + row;
    if row > stride || needed > data.maxsize as usize {
        return Err(format!("a buffer is too small for its picture ({} bytes for {}x{} at stride {})", data.maxsize, negotiated.width, negotiated.height, stride));
    }
    let mut packed = Vec::with_capacity(row * height);
    let mut copy = |base: *const u8| {
        for y in 0..height {
            // SAFETY: the bounds check above keeps every row inside the mapping
            packed.extend_from_slice(unsafe { std::slice::from_raw_parts(base.add(chunk.offset as usize + y * stride), row) });
        }
    };
    match data.kind {
        ffi::SPA_DATA_MEM_PTR => {
            if data.data.is_null() {
                return Ok(None);
            }
            copy(data.data as *const u8);
        }
        ffi::SPA_DATA_MEM_FD | ffi::SPA_DATA_DMA_BUF => {
            let dma_buf = data.kind == ffi::SPA_DATA_DMA_BUF;
            let fd = data.fd as i32;
            let length = data.mapoffset as usize + data.maxsize as usize;
            // SAFETY: a read-only shared mapping of the buffer's own descriptor, unmapped below
            let map = unsafe { libc::mmap(std::ptr::null_mut(), length, libc::PROT_READ, libc::MAP_SHARED, fd, 0) };
            if map == libc::MAP_FAILED {
                return Err(format!("a buffer couldn't be mapped ({})", std::io::Error::last_os_error()));
            }
            if dma_buf {
                dma_buf_sync(fd, SYNC_START);
            }
            copy(map.cast::<u8>().wrapping_add(data.mapoffset as usize));
            if dma_buf {
                dma_buf_sync(fd, SYNC_END);
            }
            // SAFETY: the mapping made above, with its length
            unsafe { libc::munmap(map, length) };
        }
        other => return Err(format!("a buffer has memory of a kind that isn't read (type {other})")),
    }
    Ok(Some(packed))
}

/// DMA_BUF_IOCTL_SYNC's flags for a CPU read.
const SYNC_START: u64 = 1;
const SYNC_END: u64 = 1 | (1 << 2);

/// Tells the kernel the CPU is about to read (or is done reading) a DMA-BUF, which also waits for the producer's pending writes.
fn dma_buf_sync(fd: i32, flags: u64) {
    // _IOW('b', 0, struct dma_buf_sync)
    const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
    // SAFETY: the ioctl takes a pointer to one u64 of flags, and a descriptor that isn't a DMA-BUF just fails
    unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &flags as *const u64) };
}

/// A video node being read.
pub struct VideoConsumer {
    connection: Arc<Connection>,
    node: u32,
    watch_node: bool,
    slot: Arc<Slot>,
    _stream: Stream,
}

impl VideoConsumer {
    /// Starts reading the video node `node` of `connection`. `client` names the stream in PipeWire's graph tools.
    pub fn open(connection: &Arc<Connection>, node: u32, client: &str) -> Result<VideoConsumer, String> {
        let slot = Arc::new(Slot::default());
        let handler = Handler { slot: slot.clone(), negotiated: None };
        let props = [("media.type", "Video"), ("media.category", "Capture"), ("media.role", "Production"), ("media.class", "Stream/Input/Video"), ("node.dont-reconnect", "true")];
        let config = StreamConfig { name: client, props: &props, direction: PW_DIRECTION_INPUT, target: node, flags: PW_STREAM_FLAG_AUTOCONNECT };
        let stream = Stream::connect(connection, &config, &format_offers(), Box::new(handler))?;
        Ok(VideoConsumer { connection: connection.clone(), node, watch_node: connection.node(node).is_some(), slot, _stream: stream })
    }

    /// The next picture that arrives within `timeout`: Ok(None) when nothing came, Err once the node is gone or the stream failed.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, String> {
        let deadline = Instant::now() + timeout;
        let mut state = self.slot.state.lock().map_err(|_| "the frame slot broke".to_string())?;
        loop {
            if let Some(frame) = state.frame.take() {
                return Ok(Some(frame));
            }
            if let Some(error) = state.error.clone() {
                return Err(error);
            }
            if self.connection.is_dead() {
                return Err("PipeWire went away".into());
            }
            if self.watch_node && self.connection.node(self.node).is_none() {
                return Err("the video source is gone".into());
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            state = self.slot.arrived.wait_timeout(state, (deadline - now).min(GONE_CHECK)).map_err(|_| "the frame slot broke".to_string())?.0;
        }
    }
}
