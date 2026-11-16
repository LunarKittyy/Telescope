//! Reading a video node as packed frames: offering the 32-bit RGB formats, taking whichever buffer kind the producer
//! gives (shared memory, or a DMA-BUF it allocated in a layout the CPU can read) and copying the picture out row by row.

use super::connection::Connection;
use super::ffi::{self, PwBuffer, SpaData, StreamState};
use super::pod::{self, Choice, ObjectPod, Pod, Prop, Scalar, Value};
use super::stream::{Stream, StreamConfig, StreamHandler, StreamRef, PW_DIRECTION_INPUT, PW_STREAM_FLAG_AUTOCONNECT};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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

/// A buffer taken off the stream to be read later, with everything the read needs so it never touches the pw_buffer itself.
struct Parked {
    buffer: *mut PwBuffer,
    shot: Shot,
}

// SAFETY: the pointer is only handed back to the stream (with the loop locked), never read through
unsafe impl Send for Parked {}

/// What a buffer held when it was taken: where the picture is and how its rows lie.
struct Shot {
    negotiated: Negotiated,
    /// A descriptor of our own for the buffer's memory, so the copy outlives the stream letting go of the buffer.
    fd: OwnedFd,
    dma_buf: bool,
    map_offset: usize,
    max_size: usize,
    offset: usize,
    stride: usize,
}

/// The buffer a copy is reading: given back to the stream afterwards, unless the stream took it away in the meantime.
struct InFlight {
    buffer: *mut PwBuffer,
    removed: bool,
}

// SAFETY: as Parked
unsafe impl Send for InFlight {}

#[derive(Default)]
struct SlotState {
    /// The newest picture the loop thread has parked, not yet copied out.
    parked: Option<Parked>,
    /// The picture being copied right now on the reader's thread.
    in_flight: Option<InFlight>,
    /// A picture the loop thread had to copy itself (plain memory), ready to hand out.
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

    fn remove_buffer(&mut self, _stream: &StreamRef, buffer: *mut PwBuffer) {
        let Ok(mut state) = self.slot.state.lock() else { return };
        // A copy under way owns a descriptor of its own, so it can finish; it just must not give the buffer back
        if let Some(in_flight) = state.in_flight.as_mut().filter(|f| f.buffer == buffer && !f.removed) {
            in_flight.removed = true;
        } else if state.parked.as_ref().is_some_and(|p| p.buffer == buffer) {
            state.parked = None;
        }
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
        // This runs with the loop locked, so nothing here may take long: the picture is only noted, and the reader's thread copies it
        // SAFETY: the buffer was dequeued from the stream and stays valid until it is queued again
        match unsafe { take_shot(newest, negotiated) } {
            Ok(Taken::Shot(shot)) => {
                let Ok(mut state) = self.slot.state.lock() else {
                    stream.queue(newest);
                    return;
                };
                if let Some(older) = state.parked.replace(Parked { buffer: newest, shot }) {
                    stream.queue(older.buffer);
                }
                drop(state);
                self.slot.arrived.notify_all();
            }
            Ok(Taken::Frame(frame)) => {
                stream.queue(newest);
                if let Ok(mut state) = self.slot.state.lock() {
                    state.frame = Some(frame);
                }
                self.slot.arrived.notify_all();
            }
            Ok(Taken::Nothing) => stream.queue(newest),
            Err(message) => {
                stream.queue(newest);
                self.slot.fail(message);
            }
        }
    }
}

enum Taken {
    /// A picture in memory a descriptor leads to, to be copied by the reader.
    Shot(Shot),
    /// A picture in the stream's own memory, which the stream may unmap once the buffer is given back, so it is copied right away.
    Frame(VideoFrame),
    /// The buffer carries no picture this time.
    Nothing,
}

/// Notes where the picture of a dequeued buffer is. Needs a valid dequeued pw_buffer.
unsafe fn take_shot(buffer: *mut PwBuffer, negotiated: Negotiated) -> Result<Taken, String> {
    // SAFETY: a dequeued pw_buffer points at its spa_buffer
    let spa = unsafe { (*buffer).buffer.as_ref() }.ok_or("a buffer has no data")?;
    if spa.n_datas == 0 || spa.datas.is_null() {
        return Ok(Taken::Nothing);
    }
    // SAFETY: datas holds n_datas entries
    let data: &SpaData = unsafe { &*spa.datas };
    // SAFETY: the chunk is null or the buffer's own
    let Some(chunk) = (unsafe { data.chunk.as_ref() }) else { return Ok(Taken::Nothing) };
    // A chunk flagged corrupted (bit 0) or empty is no picture
    if chunk.size == 0 || chunk.flags & 1 != 0 {
        return Ok(Taken::Nothing);
    }
    let row = negotiated.width as usize * 4;
    let stride = if chunk.stride > 0 { chunk.stride as usize } else { row };
    let height = negotiated.height as usize;
    if height == 0 {
        return Ok(Taken::Nothing);
    }
    let offset = chunk.offset as usize;
    if row > stride || offset + stride * (height - 1) + row > data.maxsize as usize {
        return Err(format!("a buffer is too small for its picture ({} bytes for {}x{} at stride {})", data.maxsize, negotiated.width, negotiated.height, stride));
    }
    match data.kind {
        ffi::SPA_DATA_MEM_PTR => {
            if data.data.is_null() {
                return Ok(Taken::Nothing);
            }
            let mut packed = vec![0u8; row * height];
            for (y, out) in packed.chunks_exact_mut(row).enumerate() {
                // SAFETY: the bounds check above keeps every row inside the buffer's memory
                out.copy_from_slice(unsafe { std::slice::from_raw_parts((data.data as *const u8).add(offset + y * stride), row) });
            }
            Ok(Taken::Frame(VideoFrame { width: negotiated.width, height: negotiated.height, format: negotiated.format, data: packed }))
        }
        ffi::SPA_DATA_MEM_FD | ffi::SPA_DATA_DMA_BUF => {
            let fd = dup_cloexec(data.fd as i32).map_err(|e| format!("a buffer's memory couldn't be held on to ({e})"))?;
            Ok(Taken::Shot(Shot {
                negotiated,
                fd,
                dma_buf: data.kind == ffi::SPA_DATA_DMA_BUF,
                map_offset: data.mapoffset as usize,
                max_size: data.maxsize as usize,
                offset,
                stride,
            }))
        }
        other => Err(format!("a buffer has memory of a kind that isn't read (type {other})")),
    }
}

/// A descriptor of our own for the same open file as `fd`, closed on exec so it can't leak into a child process while it's parked.
fn dup_cloexec(fd: i32) -> std::io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC only reads the descriptor number, and fails cleanly when it isn't open
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a fresh descriptor nothing else owns
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

/// What reading a shot came to.
enum Copied {
    Frame(VideoFrame),
    /// The kernel wouldn't sync the DMA-BUF for a CPU read, so the picture can't be trusted and is dropped.
    Skipped(std::io::Error),
}

/// Copies the picture a shot noted out into packed rows. Takes no lock of the loop's, and may wait on the producer's GPU work.
fn copy_shot(shot: &Shot) -> Result<Copied, String> {
    let Negotiated { width, height, format } = shot.negotiated;
    let length = shot.map_offset + shot.max_size;
    let fd = shot.fd.as_raw_fd();
    // SAFETY: a read-only shared mapping of the shot's own descriptor, unmapped below
    let map = unsafe { libc::mmap(std::ptr::null_mut(), length, libc::PROT_READ, libc::MAP_SHARED, fd, 0) };
    if map == libc::MAP_FAILED {
        return Err(format!("a buffer couldn't be mapped ({})", std::io::Error::last_os_error()));
    }
    let copied = copy_rows(shot, map.cast::<u8>().wrapping_add(shot.map_offset), fd);
    // SAFETY: the mapping made above, with its length
    unsafe { libc::munmap(map, length) };
    Ok(match copied {
        Ok(data) => Copied::Frame(VideoFrame { width, height, format, data }),
        Err(why) => Copied::Skipped(why),
    })
}

/// Reads the shot's rows from the mapping `base` points into (the DMA-BUF synced around the read), packed. Needs the whole shot to lie inside the mapping.
fn copy_rows(shot: &Shot, base: *const u8, fd: i32) -> std::io::Result<Vec<u8>> {
    let Negotiated { width, height, .. } = shot.negotiated;
    let row = width as usize * 4;
    if shot.dma_buf {
        dma_buf_sync(fd, SYNC_START)?;
    }
    let mut packed = vec![0u8; row * height as usize];
    for (y, out) in packed.chunks_exact_mut(row).enumerate() {
        // SAFETY: take_shot checked that every row lies inside max_size, which the mapping covers
        let src = unsafe { base.add(shot.offset + y * shot.stride) };
        if shot.dma_buf {
            // SAFETY: src holds row readable bytes (checked above) and out is row bytes long
            unsafe { copy_from_gpu(src, out) };
        } else {
            // SAFETY: as above
            out.copy_from_slice(unsafe { std::slice::from_raw_parts(src, row) });
        }
    }
    if shot.dma_buf {
        dma_buf_sync(fd, SYNC_END)?;
    }
    Ok(packed)
}

/// Copies out.len() bytes from src, which may be GPU memory mapped write-combined, where plain loads crawl (about 50 MB/s from a
/// Radeon's VRAM); SSE4.1 streaming loads read it in full lines. Needs src to hold out.len() readable bytes.
unsafe fn copy_from_gpu(src: *const u8, out: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("sse4.1") {
        // SAFETY: the CPU has SSE4.1 and the caller vouches for src
        unsafe { stream_copy(src, out) };
        return;
    }
    // SAFETY: the caller vouches for src
    out.copy_from_slice(unsafe { std::slice::from_raw_parts(src, out.len()) });
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn stream_copy(src: *const u8, out: &mut [u8]) {
    use std::arch::x86_64::{__m128i, _mm_storeu_si128, _mm_stream_load_si128};
    let len = out.len();
    let dst = out.as_mut_ptr();
    let head = src.align_offset(16).min(len);
    // SAFETY: all reads stay below src + len and writes below dst + len, and the streaming loads are 16-byte aligned past head
    unsafe {
        std::ptr::copy_nonoverlapping(src, dst, head);
        let mut i = head;
        while i + 64 <= len {
            let a = _mm_stream_load_si128(src.add(i) as *const __m128i);
            let b = _mm_stream_load_si128(src.add(i + 16) as *const __m128i);
            let c = _mm_stream_load_si128(src.add(i + 32) as *const __m128i);
            let d = _mm_stream_load_si128(src.add(i + 48) as *const __m128i);
            _mm_storeu_si128(dst.add(i) as *mut __m128i, a);
            _mm_storeu_si128(dst.add(i + 16) as *mut __m128i, b);
            _mm_storeu_si128(dst.add(i + 32) as *mut __m128i, c);
            _mm_storeu_si128(dst.add(i + 48) as *mut __m128i, d);
            i += 64;
        }
        while i + 16 <= len {
            _mm_storeu_si128(dst.add(i) as *mut __m128i, _mm_stream_load_si128(src.add(i) as *const __m128i));
            i += 16;
        }
        std::ptr::copy_nonoverlapping(src.add(i), dst.add(i), len - i);
    }
}

/// DMA_BUF_IOCTL_SYNC's flags for a CPU read.
const SYNC_START: u64 = 1;
const SYNC_END: u64 = 1 | (1 << 2);

/// How often a sync is tried again when the kernel says to.
const SYNC_TRIES: u32 = 100;
/// How many pictures in a row may be dropped for want of a sync before the reader gives up on the producer.
const MAX_SKIPPED: u32 = 30;

/// Tells the kernel the CPU is about to read (or is done reading) a DMA-BUF, which also waits for the producer's pending writes. The
/// kernel may break the wait off (EINTR, EAGAIN), which means ask again.
fn dma_buf_sync(fd: i32, flags: u64) -> std::io::Result<()> {
    // _IOW('b', 0, struct dma_buf_sync)
    const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
    retrying(|| {
        // SAFETY: the ioctl takes a pointer to one u64 of flags, and a descriptor that isn't a DMA-BUF just fails
        if unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &flags as *const u64) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })
}

/// Runs `call` again while it fails with EINTR or EAGAIN, up to SYNC_TRIES times; any other error ends it.
fn retrying(mut call: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut retries = 0;
    loop {
        match call() {
            Err(e) if retries < SYNC_TRIES && matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) => {
                retries += 1;
                std::thread::yield_now();
            }
            result => return result,
        }
    }
}

/// A video node being read.
pub struct VideoConsumer {
    connection: Arc<Connection>,
    node: u32,
    watch_node: bool,
    slot: Arc<Slot>,
    stream: Stream,
    /// How many pictures in a row were dropped because the DMA-BUF couldn't be synced.
    skipped: u32,
}

impl VideoConsumer {
    /// Starts reading the video node `node` of `connection`. `client` names the stream in PipeWire's graph tools.
    pub fn open(connection: &Arc<Connection>, node: u32, client: &str) -> Result<VideoConsumer, String> {
        let slot = Arc::new(Slot::default());
        let handler = Handler { slot: slot.clone(), negotiated: None };
        let props = [("media.type", "Video"), ("media.category", "Capture"), ("media.role", "Production"), ("media.class", "Stream/Input/Video"), ("node.dont-reconnect", "true")];
        let config = StreamConfig { name: client, props: &props, direction: PW_DIRECTION_INPUT, target: node, flags: PW_STREAM_FLAG_AUTOCONNECT };
        let stream = Stream::connect(connection, &config, &format_offers(), Box::new(handler))?;
        Ok(VideoConsumer { connection: connection.clone(), node, watch_node: connection.node(node).is_some(), slot, stream, skipped: 0 })
    }

    /// The next picture that arrives within `timeout`: Ok(None) when nothing came, Err once the node is gone or the stream failed.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, String> {
        let deadline = Instant::now() + timeout;
        let broke = || "the frame slot broke".to_string();
        let mut state = self.slot.state.lock().map_err(|_| broke())?;
        loop {
            if let Some(frame) = state.frame.take() {
                return Ok(Some(frame));
            }
            if let Some(parked) = state.parked.take() {
                state.in_flight = Some(InFlight { buffer: parked.buffer, removed: false });
                drop(state);
                let copied = copy_shot(&parked.shot);
                self.give_back(parked.buffer);
                state = self.slot.state.lock().map_err(|_| broke())?;
                match copied {
                    Ok(Copied::Frame(frame)) => {
                        self.skipped = 0;
                        return Ok(Some(frame));
                    }
                    Ok(Copied::Skipped(why)) => {
                        // This picture is dropped and the next one tried, unless it keeps happening
                        if self.skipped == 0 {
                            log::warn!("A DMA-BUF couldn't be synced for reading, so its picture was dropped: {why}");
                        }
                        self.skipped += 1;
                        if self.skipped >= MAX_SKIPPED {
                            state.error.get_or_insert(format!("the producer's pictures can't be read safely ({why})"));
                        }
                        continue;
                    }
                    Err(message) => {
                        state.error.get_or_insert(message);
                        continue;
                    }
                }
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
            state = self.slot.arrived.wait_timeout(state, (deadline - now).min(GONE_CHECK)).map_err(|_| broke())?.0;
        }
    }

    /// Hands the buffer that was just copied back to the stream, unless the stream has taken it away since.
    fn give_back(&self, buffer: *mut PwBuffer) {
        // The slot is looked at with the loop locked, so a buffer can't be removed between the look and the queueing
        self.stream.locked(|stream| {
            let removed = match self.slot.state.lock() {
                Ok(mut state) => state.in_flight.take().is_none_or(|f| f.removed),
                Err(_) => true,
            };
            if !removed {
                stream.queue(buffer);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind, Write};
    use std::os::fd::AsFd;

    fn memfd(name: &str, bytes: &[u8]) -> OwnedFd {
        let name = std::ffi::CString::new(name).unwrap();
        // SAFETY: a NUL-terminated name; the flags ask for a close-on-exec descriptor
        let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0, "memfd_create failed: {}", Error::last_os_error());
        // SAFETY: a fresh descriptor nothing else owns
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut file = std::fs::File::from(fd.as_fd().try_clone_to_owned().unwrap());
        file.write_all(bytes).unwrap();
        fd
    }

    fn flags_of(fd: i32) -> i32 {
        // SAFETY: F_GETFD only reads the descriptor's flags
        unsafe { libc::fcntl(fd, libc::F_GETFD) }
    }

    fn shot(fd: OwnedFd, dma_buf: bool) -> Shot {
        Shot { negotiated: Negotiated { width: 4, height: 2, format: VideoFormat::Bgra }, fd, dma_buf, map_offset: 0, max_size: 32, offset: 0, stride: 16 }
    }

    #[test]
    fn a_held_descriptor_is_closed_on_exec_even_when_the_original_is_not() {
        // SAFETY: a plain /dev/null descriptor opened without close-on-exec, owned here
        let plain = unsafe { OwnedFd::from_raw_fd(libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY)) };
        assert_eq!(flags_of(plain.as_raw_fd()) & libc::FD_CLOEXEC, 0, "the test's original should lack close-on-exec");
        let held = dup_cloexec(plain.as_raw_fd()).unwrap();
        assert_ne!(held.as_raw_fd(), plain.as_raw_fd());
        assert_ne!(flags_of(held.as_raw_fd()) & libc::FD_CLOEXEC, 0, "a parked frame's descriptor must not leak into child processes");
        assert!(dup_cloexec(-1).is_err());
    }

    #[test]
    fn a_sync_the_kernel_breaks_off_is_asked_again() {
        for kind in [libc::EINTR, libc::EAGAIN] {
            let mut calls = 0;
            let result = retrying(|| {
                calls += 1;
                if calls < 3 { Err(Error::from_raw_os_error(kind)) } else { Ok(()) }
            });
            assert!(result.is_ok() && calls == 3, "errno {kind}: {result:?} after {calls} calls");
        }
    }

    #[test]
    fn a_sync_that_keeps_being_broken_off_gives_up_and_other_errors_end_it_at_once() {
        let mut calls = 0;
        let result = retrying(|| {
            calls += 1;
            Err(Error::from(ErrorKind::Interrupted))
        });
        assert!(result.is_err());
        assert_eq!(calls, SYNC_TRIES + 1);
        let mut calls = 0;
        let result = retrying(|| {
            calls += 1;
            Err(Error::from_raw_os_error(libc::EINVAL))
        });
        assert_eq!((result.is_err(), calls), (true, 1));
    }

    #[test]
    fn a_dma_buf_that_wont_sync_drops_the_picture_and_still_unmaps() {
        // A memfd is no DMA-BUF, so the sync ioctl fails for good
        let name = format!("telescope-sync-test-{}", std::process::id());
        let fd = memfd(&name, &[7u8; 32]);
        let copied = copy_shot(&shot(fd, true)).expect("a sync failure isn't an error of the reader");
        assert!(matches!(copied, Copied::Skipped(_)));
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(!maps.contains(&name), "the mapping was left behind");
    }

    #[test]
    fn a_memory_picture_is_copied_row_by_row() {
        let bytes: Vec<u8> = (0u8..32).collect();
        let fd = memfd(&format!("telescope-copy-test-{}", std::process::id()), &bytes);
        let Copied::Frame(frame) = copy_shot(&shot(fd, false)).unwrap() else { panic!("a plain picture should copy") };
        assert_eq!((frame.width, frame.height), (4, 2));
        assert_eq!(frame.data, bytes);
    }
}
