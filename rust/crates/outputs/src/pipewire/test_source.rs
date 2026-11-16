//! A fake video producer for tests, negotiating the way libfunnel does (the library behind Spout2PW): one fixed-size
//! format per pixel format, a modifier list the producer fixes itself, DMA-BUF buffers it allocates, a driving stream.

use super::ffi::{PwBuffer, SPA_DATA_DMA_BUF, SPA_DATA_MEM_FD};
use super::pod::{self, Choice, ObjectPod, Pod, Prop, Scalar, Value};
use super::{Connection, Stream, StreamConfig, StreamHandler, StreamRef, VideoFormat, PW_DIRECTION_OUTPUT, PW_STREAM_FLAG_ALLOC_BUFFERS, PW_STREAM_FLAG_DRIVER, SPA_ID_INVALID};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 4;

/// Where the fake's pictures live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Memory {
    /// Linear DMA-BUFs from the kernel's system heap, as Spout2PW's are.
    DmaBuf,
    /// Plain shared memory the stream allocates.
    MemFd,
}

pub struct SourceSpec {
    /// The stream's name and every property it gets (media.class, node.name, ...).
    pub name: String,
    pub props: Vec<(String, String)>,
    pub width: u32,
    pub height: u32,
    pub format: VideoFormat,
    pub memory: Memory,
}

/// The pixel at (x, y) of the test picture, in the format's byte order: left half one colour, right half another, bottom rows a third.
pub fn test_pixel(format: VideoFormat, width: u32, height: u32, x: u32, y: u32) -> [u8; 4] {
    let (r, g, b) = if y >= height - height / 4 {
        (10, 200, 30)
    } else if x < width / 2 {
        (220, 40, 20)
    } else {
        (20, 60, 230)
    };
    match format {
        VideoFormat::Bgra => [b, g, r, 255],
        VideoFormat::Bgrx => [b, g, r, 0],
        VideoFormat::Rgba => [r, g, b, 255],
        VideoFormat::Rgbx => [r, g, b, 0],
    }
}

/// Fills `rows` rows of `stride` bytes with the test picture.
fn paint(target: &mut [u8], spec: &SourceSpec, stride: usize) {
    for y in 0..spec.height {
        for x in 0..spec.width {
            let at = y as usize * stride + x as usize * 4;
            target[at..at + 4].copy_from_slice(&test_pixel(spec.format, spec.width, spec.height, x, y));
        }
    }
}

fn enum_format(spec: &SourceSpec, fixed_modifier: bool) -> Pod {
    let mut props = vec![
        Prop { key: pod::FORMAT_MEDIA_TYPE, flags: 0, value: Value::One(Scalar::Id(pod::MEDIA_TYPE_VIDEO)) },
        Prop { key: pod::FORMAT_MEDIA_SUBTYPE, flags: 0, value: Value::One(Scalar::Id(pod::MEDIA_SUBTYPE_RAW)) },
        Prop { key: pod::FORMAT_VIDEO_SIZE, flags: 0, value: Value::One(Scalar::Rect(spec.width, spec.height)) },
        Prop { key: pod::FORMAT_VIDEO_FRAMERATE, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Frac(30, 1), Scalar::Frac(1, 1), Scalar::Frac(1000, 1)]) },
        Prop { key: pod::FORMAT_VIDEO_FORMAT, flags: 0, value: Value::One(Scalar::Id(spec.format.spa_id())) },
    ];
    if spec.memory == Memory::DmaBuf {
        let flags = if fixed_modifier { pod::PROP_MANDATORY } else { pod::PROP_MANDATORY | pod::PROP_DONT_FIXATE };
        props.push(Prop { key: pod::FORMAT_VIDEO_MODIFIER, flags, value: Value::Choice(Choice::Enum, vec![Scalar::Long(0), Scalar::Long(0)]) });
    }
    Pod::object(pod::OBJECT_FORMAT, pod::PARAM_ENUM_FORMAT, &props)
}

fn buffers(spec: &SourceSpec) -> Pod {
    let int = |v: i32| Value::One(Scalar::Int(v));
    let kind = if spec.memory == Memory::DmaBuf { 1 << SPA_DATA_DMA_BUF } else { 1 << SPA_DATA_MEM_FD };
    let mut props = vec![
        Prop { key: pod::BUFFERS_BUFFERS, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Int(4), Scalar::Int(2), Scalar::Int(8)]) },
        Prop { key: pod::BUFFERS_BLOCKS, flags: 0, value: int(1) },
        Prop { key: pod::BUFFERS_DATA_TYPE, flags: 0, value: Value::Choice(Choice::Flags, vec![Scalar::Int(kind)]) },
    ];
    if spec.memory == Memory::MemFd {
        props.push(Prop { key: pod::BUFFERS_SIZE, flags: 0, value: int((spec.width * spec.height * 4) as i32) });
        props.push(Prop { key: pod::BUFFERS_STRIDE, flags: 0, value: int((spec.width * 4) as i32) });
        props.push(Prop { key: pod::BUFFERS_ALIGN, flags: 0, value: int(16) });
    }
    Pod::object(pod::OBJECT_PARAM_BUFFERS, pod::PARAM_BUFFERS, &props)
}

/// A linear buffer of `len` bytes from the system heap.
fn heap_buffer(len: usize) -> Result<OwnedFd, String> {
    #[repr(C)]
    struct Allocation {
        len: u64,
        fd: u32,
        fd_flags: u32,
        heap_flags: u64,
    }
    // _IOWR('H', 0, struct dma_heap_allocation_data)
    const DMA_HEAP_IOCTL_ALLOC: libc::c_ulong = 0xC018_4800;
    let heap = std::fs::OpenOptions::new().read(true).write(true).open("/dev/dma_heap/system").map_err(|e| format!("no DMA-BUF heap to allocate from: {e}"))?;
    let mut request = Allocation { len: len as u64, fd: 0, fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32, heap_flags: 0 };
    // SAFETY: the ioctl fills the Allocation passed, which is repr(C) and matches the kernel's struct
    let result = unsafe { libc::ioctl(heap.as_raw_fd(), DMA_HEAP_IOCTL_ALLOC, &mut request as *mut Allocation) };
    if result < 0 {
        return Err(format!("the DMA-BUF heap refused a buffer: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: the kernel just returned this descriptor, which nothing else owns
    Ok(unsafe { OwnedFd::from_raw_fd(request.fd as i32) })
}

struct Producer {
    spec: Arc<SourceSpec>,
}

impl StreamHandler for Producer {
    fn param_changed(&mut self, stream: &StreamRef, id: u32, format: Option<ObjectPod>) {
        let (true, Some(format)) = (id == pod::PARAM_FORMAT, format) else { return };
        let modifier = format.prop(pod::FORMAT_VIDEO_MODIFIER);
        if modifier.is_some_and(|m| m.flags & pod::PROP_DONT_FIXATE != 0) {
            // The way libfunnel settles it: pick the modifier and offer the format again with it fixed
            stream.update_params(&[enum_format(&self.spec, true)]);
        } else {
            stream.update_params(&[buffers(&self.spec)]);
        }
    }

    fn add_buffer(&mut self, _stream: &StreamRef, buffer: *mut PwBuffer) {
        if self.spec.memory != Memory::DmaBuf {
            return;
        }
        let stride = (self.spec.width as usize * 4).next_multiple_of(256);
        let len = stride * self.spec.height as usize;
        let Ok(fd) = heap_buffer(len) else { return };
        // SAFETY: a shared writable mapping of the fresh descriptor, unmapped below
        let map = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
        if map == libc::MAP_FAILED {
            return;
        }
        // SAFETY: the mapping is len bytes and nothing else uses it yet
        paint(unsafe { std::slice::from_raw_parts_mut(map.cast::<u8>(), len) }, &self.spec, stride);
        // SAFETY: the mapping made above
        unsafe { libc::munmap(map, len) };
        // SAFETY: the buffer is the stream's own, valid inside this callback, and has one data as the Buffers param asked
        unsafe {
            let data = &mut *(*(*buffer).buffer).datas;
            data.kind = SPA_DATA_DMA_BUF;
            data.fd = std::os::fd::IntoRawFd::into_raw_fd(fd) as i64;
            data.mapoffset = 0;
            data.maxsize = len as u32;
            data.data = std::ptr::null_mut();
            let chunk = &mut *data.chunk;
            chunk.offset = 0;
            chunk.size = len as u32;
            chunk.stride = stride as i32;
        }
    }

    fn remove_buffer(&mut self, _stream: &StreamRef, buffer: *mut PwBuffer) {
        if self.spec.memory == Memory::DmaBuf {
            // SAFETY: the buffer is valid inside this callback, and its fd is the one add_buffer stored
            let fd = unsafe { (*(*(*buffer).buffer).datas).fd };
            if fd >= 0 {
                // SAFETY: the descriptor was made in add_buffer and is closed once
                unsafe { libc::close(fd as i32) };
            }
        }
    }

    fn process(&mut self, stream: &StreamRef) {
        let buffer = stream.dequeue();
        if buffer.is_null() {
            return;
        }
        if self.spec.memory == Memory::MemFd {
            // SAFETY: the buffer is the stream's own with one mapped data (MAP_BUFFERS), valid until it is queued
            unsafe {
                let data = &mut *(*(*buffer).buffer).datas;
                if !data.data.is_null() {
                    let stride = self.spec.width as usize * 4;
                    paint(std::slice::from_raw_parts_mut(data.data.cast::<u8>(), data.maxsize as usize), &self.spec, stride);
                    let chunk = &mut *data.chunk;
                    chunk.offset = 0;
                    chunk.size = (stride * self.spec.height as usize) as u32;
                    chunk.stride = stride as i32;
                }
            }
        }
        stream.queue(buffer);
    }
}

/// A running fake source, gone when dropped.
pub struct FakeSource {
    stream: Arc<Stream>,
    running: Arc<AtomicBool>,
    ticker: Option<std::thread::JoinHandle<()>>,
}

impl FakeSource {
    /// Err when this machine can't give the buffers the spec asks for (no DMA-BUF heap), so a test can skip.
    pub fn start(connection: &Arc<Connection>, spec: SourceSpec) -> Result<FakeSource, String> {
        if spec.memory == Memory::DmaBuf {
            heap_buffer(4096)?;
        }
        let spec = Arc::new(spec);
        let props: Vec<(&str, &str)> = spec.props.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let config = StreamConfig { name: &spec.name, props: &props, direction: PW_DIRECTION_OUTPUT, target: SPA_ID_INVALID, flags: PW_STREAM_FLAG_DRIVER | PW_STREAM_FLAG_ALLOC_BUFFERS | PW_STREAM_FLAG_MAP_BUFFERS };
        let stream = Arc::new(Stream::connect(connection, &config, &[enum_format(&spec, false)], Box::new(Producer { spec: spec.clone() }))?);
        let running = Arc::new(AtomicBool::new(true));
        let ticker = {
            let (stream, running) = (stream.clone(), running.clone());
            std::thread::spawn(move || {
                while running.load(Ordering::SeqCst) {
                    stream.trigger_process();
                    std::thread::sleep(Duration::from_millis(33));
                }
            })
        };
        Ok(FakeSource { stream, running, ticker: Some(ticker) })
    }
}

impl Drop for FakeSource {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let _ = &self.stream;
    }
}
