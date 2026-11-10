//! libavcodec underneath the decoders: one codec context and what it needs, and decoded pictures copied out packed.

use ffmpeg_sys_next as ff;
use std::ptr;
use crate::picture::{exif_orientation, kind, oriented, Kind};
use crate::transfers;
use crate::yuv;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::time::{monotonic_ns, span};

/// A graphics card decoder FFmpeg can open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hw {
    Vaapi,
    D3d11va,
    Cuda,
}

impl Hw {
    pub fn name(self) -> &'static str {
        match self {
            Hw::Vaapi => "vaapi",
            Hw::D3d11va => "d3d11va",
            Hw::Cuda => "cuda",
        }
    }

    fn device_type(self) -> ff::AVHWDeviceType {
        match self {
            Hw::Vaapi => ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            Hw::D3d11va => ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            Hw::Cuda => ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
        }
    }

    fn pixel_format(self) -> ff::AVPixelFormat {
        match self {
            Hw::Vaapi => ff::AVPixelFormat::AV_PIX_FMT_VAAPI,
            Hw::D3d11va => ff::AVPixelFormat::AV_PIX_FMT_D3D11,
            Hw::Cuda => ff::AVPixelFormat::AV_PIX_FMT_CUDA,
        }
    }

    fn from_pixel_format(fmt: i32) -> Option<Hw> {
        [Hw::Vaapi, Hw::D3d11va, Hw::Cuda].into_iter().find(|h| h.pixel_format() as i32 == fmt)
    }
}

/// A failure to force on a card decoder, so the fallback can be seen working on a real stream:
/// TELESCOPE_HW_FAULT=open or TELESCOPE_HW_FAULT=<kind>@<frames>, after that many good frames (0 when left out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forced {
    /// The device doesn't open.
    Open,
    /// Every packet is refused as bad data.
    Reject,
    /// The decoder itself errors.
    Broken,
    /// The picture can't be copied off the card.
    Copy,
    /// The copied picture has no size.
    Empty,
    /// The copied picture is a surface nobody wrote to: all zeros.
    Blank,
    /// Packets go in and nothing comes out.
    Silent,
}

impl Forced {
    /// What TELESCOPE_HW_FAULT asks for.
    pub fn asked() -> Option<(Forced, u32)> {
        Forced::parse(&std::env::var("TELESCOPE_HW_FAULT").ok()?)
    }

    fn parse(text: &str) -> Option<(Forced, u32)> {
        let (kind, after) = text.split_once('@').unwrap_or((text, "0"));
        let kind = match kind {
            "open" => Forced::Open,
            "reject" => Forced::Reject,
            "broken" => Forced::Broken,
            "copy" => Forced::Copy,
            "empty" => Forced::Empty,
            "blank" => Forced::Blank,
            "silent" => Forced::Silent,
            _ => return None,
        };
        Some((kind, after.parse().ok()?))
    }
}

/// Why decoding a piece of the stream went wrong.
#[derive(Debug, PartialEq, Eq)]
pub enum Fault {
    /// The data isn't decodable as it is (joined mid-GOP, a lost reference): skip to the next keyframe.
    Invalid,
    /// The decoder itself fell over (a graphics card driver refusing the stream).
    Broken(String),
}

fn err_text(code: i32) -> String {
    let mut buf = [0 as std::ffi::c_char; 128];
    // SAFETY: buf is big enough and av_strerror always terminates it
    unsafe {
        ff::av_strerror(code, buf.as_mut_ptr(), buf.len());
        std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

unsafe extern "C" fn pick_hw(ctx: *mut ff::AVCodecContext, mut formats: *const ff::AVPixelFormat) -> ff::AVPixelFormat {
    // The wanted format is kept in opaque as an integer; nothing else uses it
    let want = (*ctx).opaque as usize as i32;
    while *formats != ff::AVPixelFormat::AV_PIX_FMT_NONE {
        if *formats as i32 == want {
            return *formats;
        }
        formats = formats.add(1);
    }
    // No software fallback: a card that can't take this stream fails, and the decoder switches to software itself
    ff::AVPixelFormat::AV_PIX_FMT_NONE
}

/// How software H.264 spreads its work: how many threads, and whether they take whole pictures (each adds a picture of delay) or slices.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Threads {
    pub count: i32,
    pub whole_frames: bool,
}

/// Slice threads for software H.264: a stream of several slices a picture decodes up to this many times faster (4K in 4 slices: 13 ms
/// with one thread, 8.5 with 2, 6.8 with 4, no better with 8), one slice goes the same speed with any. Whole-picture threads would
/// decode single-slice streams faster too, but each one holds back a picture, so they stay off.
fn software_h264_threads() -> Threads {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    Threads { count: cores.clamp(1, 4) as i32, whole_frames: false }
}

/// One libavcodec decoder with its parser (for H.264) and buffers.
pub struct Codec {
    ctx: *mut ff::AVCodecContext,
    parser: *mut ff::AVCodecParserContext,
    packet: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
    sw_frame: *mut ff::AVFrame,
    // The newest picture decode_newest has finished, still on the decoder's side until take_newest reads it
    held: *mut ff::AVFrame,
    hw_device: *mut ff::AVBufferRef,
    sws: *mut ff::SwsContext,
    pub hw: Option<Hw>,
    /// Whether to ask the card for I420 at the copy. Off: the card's own NV12 is taken and split here, since a driver's conversion to
    /// I420 runs on the graphics card's compute queue and waits behind a game's work (48 ms a 4K picture beside one, 9 ms as NV12).
    /// Only tests and benches turn it on, to compare.
    pub(crate) card_gives_i420: bool,
    /// What to fail on purpose, and how many pictures have been good so far.
    pub forced: Option<(Forced, u32)>,
    pictures: u32,
    /// Tests: each picture's copy off the card, first and then again with the decoder surely done, in nanoseconds.
    #[cfg(test)]
    pub copy_ns: Option<Vec<(u64, u64)>>,
    /// Tests: this many pictures are refused as a card that can't take the stream would.
    #[cfg(test)]
    pub refusals: u32,
    // A JPEG decoder: JFIF pictures are always full range BT.601, whatever the frame says (OpenCV decodes them so)
    jpeg: bool,
    converter: yuv::Converter,
    // Where swscale writes the pictures it converts, padded for its SIMD loops
    sws_out: Padded,
    // Copies of the bytes handed to FFmpeg with the zeroed tail its bitstream readers may read into
    parse_buf: Vec<u8>,
    unit_buf: Vec<u8>,
}

/// Copies data into buf followed by the AV_INPUT_BUFFER_PADDING_SIZE zero bytes FFmpeg requires after any input.
fn padded<'a>(buf: &'a mut Vec<u8>, data: &[u8]) -> &'a [u8] {
    buf.clear();
    buf.extend_from_slice(data);
    buf.resize(data.len() + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize, 0);
    buf
}

// SAFETY: a Codec is used from one thread at a time; FFmpeg contexts aren't tied to the thread that made them
unsafe impl Send for Codec {}

impl Codec {
    /// H.264 with low delay, in software (two slice threads) or on hw with no software fallback.
    pub fn h264(hw: Option<Hw>) -> Result<Codec, String> {
        Codec::open(ff::AVCodecID::AV_CODEC_ID_H264, hw, true)
    }

    pub fn mjpeg() -> Result<Codec, String> {
        Codec::open(ff::AVCodecID::AV_CODEC_ID_MJPEG, None, false)
    }

    /// VP8 in software, one whole frame per packet (the Browser camera from a browser without H.264).
    pub fn vp8() -> Result<Codec, String> {
        Codec::open(ff::AVCodecID::AV_CODEC_ID_VP8, None, false)
    }

    fn open(id: ff::AVCodecID, hw: Option<Hw>, parse: bool) -> Result<Codec, String> {
        Codec::open_threaded(id, hw, parse, software_h264_threads())
    }

    /// Software H.264 as a bench or test wants it: this many threads, slice threading (low delay) or whole-frame threading.
    #[cfg(test)]
    pub(crate) fn h264_threaded(threads: Threads) -> Result<Codec, String> {
        Codec::open_threaded(ff::AVCodecID::AV_CODEC_ID_H264, None, true, threads)
    }

    fn open_threaded(id: ff::AVCodecID, hw: Option<Hw>, parse: bool, threads: Threads) -> Result<Codec, String> {
        // SAFETY: plain libavcodec setup; every pointer is checked before use and freed in Drop
        unsafe {
            let codec = ff::avcodec_find_decoder(id);
            if codec.is_null() {
                return Err(format!("this FFmpeg has no {id:?} decoder"));
            }
            let mut c = Codec {
                ctx: ff::avcodec_alloc_context3(codec),
                parser: if parse { ff::av_parser_init(id as i32) } else { ptr::null_mut() },
                packet: ff::av_packet_alloc(),
                frame: ff::av_frame_alloc(),
                sw_frame: ff::av_frame_alloc(),
                held: ff::av_frame_alloc(),
                hw_device: ptr::null_mut(),
                sws: ptr::null_mut(),
                hw,
                card_gives_i420: false,
                forced: if hw.is_some() { Forced::asked() } else { None },
                pictures: 0,
                #[cfg(test)]
                copy_ns: None,
                #[cfg(test)]
                refusals: 0,
                jpeg: id == ff::AVCodecID::AV_CODEC_ID_MJPEG,
                converter: yuv::Converter::default(),
                sws_out: Padded::default(),
                parse_buf: Vec::new(),
                unit_buf: Vec::new(),
            };
            if c.ctx.is_null() || c.packet.is_null() || c.frame.is_null() || c.sw_frame.is_null() || c.held.is_null() || (parse && c.parser.is_null()) {
                return Err("out of memory opening a decoder".into());
            }
            // Show each frame as soon as it's decoded instead of holding some back for reordering (h264_reader.py)
            if !threads.whole_frames {
                (*c.ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
            }
            match hw {
                Some(h) => {
                    let r = ff::av_hwdevice_ctx_create(&mut c.hw_device, h.device_type(), ptr::null(), ptr::null_mut(), 0);
                    if r < 0 {
                        return Err(format!("{} didn't open: {}", h.name(), err_text(r)));
                    }
                    #[cfg(windows)]
                    if h == Hw::D3d11va {
                        raise_gpu_priority(c.hw_device);
                    }
                    (*c.ctx).hw_device_ctx = ff::av_buffer_ref(c.hw_device);
                    (*c.ctx).opaque = h.pixel_format() as i32 as usize as *mut _;
                    (*c.ctx).get_format = Some(pick_hw);
                    // The picture decode_newest holds is a surface the decoder can't use meanwhile
                    (*c.ctx).extra_hw_frames = 1;
                }
                None if parse => {
                    (*c.ctx).thread_count = threads.count;
                    if threads.whole_frames {
                        (*c.ctx).thread_type = ff::FF_THREAD_FRAME as i32;
                    }
                }
                None => {}
            }
            let r = ff::avcodec_open2(c.ctx, codec, ptr::null_mut());
            if r < 0 {
                return Err(format!("couldn't open the decoder: {}", err_text(r)));
            }
            Ok(c)
        }
    }

    /// Splits stream bytes into access units (H.264), calling unit for each one the parser finishes.
    pub fn parse(&mut self, data: &[u8], mut unit: impl FnMut(&mut Codec, &[u8])) {
        // Only a codec opened with a parser can split anything; the others have none to call
        if self.parser.is_null() {
            return;
        }
        let mut buf = std::mem::take(&mut self.parse_buf);
        let len = data.len();
        let input = padded(&mut buf, data);
        let mut at = 0;
        // SAFETY: input has FFmpeg's padding after len; the parser hands back a pointer into its own (padded) buffer or
        // into input, valid until the next parse call, and unit gets a copy
        unsafe {
            while at < len {
                let mut out: *mut u8 = ptr::null_mut();
                let mut out_len: i32 = 0;
                let used = ff::av_parser_parse2(self.parser, self.ctx, &mut out, &mut out_len, input[at..].as_ptr(), (len - at).min(i32::MAX as usize) as i32,
                                                ff::AV_NOPTS_VALUE, ff::AV_NOPTS_VALUE, 0);
                if used < 0 {
                    break;
                }
                at += used as usize;
                if out_len > 0 {
                    let piece = std::slice::from_raw_parts(out, out_len as usize).to_vec();
                    unit(self, &piece);
                }
            }
        }
        self.parse_buf = buf;
    }

    /// Decodes one access unit (or one whole JPEG); every frame it finishes goes to done.
    pub fn decode(&mut self, unit: &[u8], mut done: impl FnMut(Frame)) -> Result<u32, Fault> {
        self.feed(unit, Some(&mut done))
    }

    /// Decodes one access unit, keeping the newest picture it finishes on the decoder's side for take_newest. A picture that a
    /// later one replaces is never downloaded or converted.
    pub fn decode_newest(&mut self, unit: &[u8]) -> Result<u32, Fault> {
        self.feed(unit, None)
    }

    /// The newest picture decode_newest kept, as a frame, and nothing is kept after it.
    pub fn take_newest(&mut self) -> Result<Option<Frame>, Fault> {
        // SAFETY: held and frame are this codec's own frames; move_ref leaves held empty, and take_frame reads the picture in frame
        unsafe {
            if (*self.held).buf[0].is_null() {
                return Ok(None);
            }
            ff::av_frame_unref(self.frame);
            ff::av_frame_move_ref(self.frame, self.held);
            self.take_frame().map(Some)
        }
    }

    /// Lets go of the picture decode_newest kept.
    pub fn drop_newest(&mut self) {
        // SAFETY: held is this codec's own frame
        unsafe { ff::av_frame_unref(self.held) }
    }

    fn feed(&mut self, unit: &[u8], mut done: Option<&mut dyn FnMut(Frame)>) -> Result<u32, Fault> {
        #[cfg(test)]
        if self.refusals > 0 {
            self.refusals -= 1;
            return Err(Fault::Invalid);
        }
        match self.forced_now() {
            Some(Forced::Reject) => return Err(Fault::Invalid),
            Some(Forced::Broken) => return Err(Fault::Broken("forced by TELESCOPE_HW_FAULT".into())),
            Some(Forced::Silent) => return Ok(0),
            _ => {}
        }
        let mut count = 0;
        let input = padded(&mut self.unit_buf, unit).as_ptr();
        // SAFETY: the packet points at a padded copy of unit for the length of send_packet, which copies it (the packet
        // isn't refcounted)
        unsafe {
            (*self.packet).data = input as *mut u8;
            (*self.packet).size = unit.len() as i32;
            let began = monotonic_ns();
            let r = ff::avcodec_send_packet(self.ctx, self.packet);
            span("decode", began);
            (*self.packet).data = ptr::null_mut();
            (*self.packet).size = 0;
            if r == ff::AVERROR_INVALIDDATA {
                return Err(Fault::Invalid);
            }
            if r < 0 && r != ff::AVERROR(libc::EAGAIN) {
                return Err(Fault::Broken(err_text(r)));
            }
            loop {
                let began = monotonic_ns();
                let r = ff::avcodec_receive_frame(self.ctx, self.frame);
                span("receive", began);
                if r == ff::AVERROR(libc::EAGAIN) || r == ff::AVERROR_EOF {
                    break;
                }
                if r == ff::AVERROR_INVALIDDATA {
                    return Err(Fault::Invalid);
                }
                if r < 0 {
                    return Err(Fault::Broken(err_text(r)));
                }
                count += 1;
                match done.as_mut() {
                    Some(done) => {
                        let began = monotonic_ns();
                        let frame = self.take_frame()?;
                        span("take_frame", began);
                        done(frame);
                    }
                    None => {
                        // A picture this one replaces still counts as one the card gave
                        if !(*self.held).buf[0].is_null() {
                            self.pictures = self.pictures.saturating_add(1);
                        }
                        ff::av_frame_unref(self.held);
                        ff::av_frame_move_ref(self.held, self.frame);
                    }
                }
            }
        }
        Ok(count)
    }

    fn forced_now(&self) -> Option<Forced> {
        self.forced.filter(|(_, after)| self.pictures >= *after).map(|(kind, _)| kind)
    }

    /// The card's picture in sw_frame, in main memory.
    unsafe fn copy_off_the_card(&mut self) -> Result<(), Fault> {
        ff::av_frame_unref(self.sw_frame);
        // The card's own NV12 is split in take_frame, unless a test asked for I420
        if self.card_gives_i420 {
            (*self.sw_frame).format = ff::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
        }
        #[cfg(test)]
        let started = std::time::Instant::now();
        let began = monotonic_ns();
        let mut r = ff::av_hwframe_transfer_data(self.sw_frame, self.frame, 0);
        span("transfer", began);
        #[cfg(test)]
        if let Some(times) = &mut self.copy_ns {
            let first = started.elapsed().as_nanos() as u64;
            let again = std::time::Instant::now();
            ff::av_hwframe_transfer_data(self.sw_frame, self.frame, 0);
            times.push((first, again.elapsed().as_nanos() as u64));
        }
        if r < 0 && self.card_gives_i420 {
            ff::av_frame_unref(self.sw_frame);
            r = ff::av_hwframe_transfer_data(self.sw_frame, self.frame, 0);
            self.card_gives_i420 = r < 0;
        }
        if self.forced_now() == Some(Forced::Copy) {
            r = ff::AVERROR(libc::EIO);
        }
        if r < 0 {
            return Err(Fault::Broken(format!("couldn't copy the picture off the card: {}", err_text(r))));
        }
        transfers::decoded_down();
        Ok(())
    }

    /// The decoded picture as one packed I420 frame (NV12 from the card is split into U and V).
    unsafe fn take_frame(&mut self) -> Result<Frame, Fault> {
        let mut src = self.frame;
        if Hw::from_pixel_format((*src).format).is_some() {
            self.copy_off_the_card()?;
            src = self.sw_frame;
            match self.forced_now() {
                Some(Forced::Empty) => (*src).width = 0,
                Some(Forced::Blank) => {
                    for (plane, rows) in [(0, (*src).height), (1, (*src).height / 2)] {
                        ptr::write_bytes((*src).data[plane], 0, ((*src).linesize[plane] * rows) as usize);
                    }
                }
                _ => {}
            }
            if (*src).width > 0 && unwritten(src) {
                return Err(Fault::Invalid);
            }
        }
        let (w, h) = ((*src).width as usize, (*src).height as usize);
        if w == 0 || h == 0 {
            return Err(Fault::Invalid);
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let fmt = (*src).format;
        let mut i420 = Vec::with_capacity(w * h + 2 * cw * ch);
        let copy_plane = |out: &mut Vec<u8>, data: *const u8, stride: i32, pw: usize, ph: usize| {
            for row in 0..ph {
                out.extend_from_slice(std::slice::from_raw_parts(data.offset(row as isize * stride as isize), pw));
            }
        };
        // A JPEG is always BT.601 over the whole 0..255; a stream says so in the headers on the decoder's own frame, read as plain integers
        let int_of = |field: *const i32| field.read();
        let matrix = if self.jpeg { 5 } else { int_of(ptr::addr_of!((*self.frame).colorspace).cast()) };
        let mut full_range = self.jpeg || fmt == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32
            || int_of(ptr::addr_of!((*self.frame).color_range).cast()) == ff::AVColorRange::AVCOL_RANGE_JPEG as i32;
        let yuv420 = [ff::AVPixelFormat::AV_PIX_FMT_YUV420P as i32, ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32];
        if yuv420.contains(&fmt) {
            copy_plane(&mut i420, (*src).data[0], (*src).linesize[0], w, h);
            copy_plane(&mut i420, (*src).data[1], (*src).linesize[1], cw, ch);
            copy_plane(&mut i420, (*src).data[2], (*src).linesize[2], cw, ch);
        } else if fmt == ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
            copy_plane(&mut i420, (*src).data[0], (*src).linesize[0], w, h);
            let (uv, stride) = ((*src).data[1], (*src).linesize[1] as isize);
            // The two planes are filled in place, a row of interleaved pairs at a time
            let chroma_at = i420.len();
            i420.resize(chroma_at + 2 * cw * ch, 0);
            let (u, v) = i420[chroma_at..].split_at_mut(cw * ch);
            for (row, (u, v)) in u.chunks_exact_mut(cw).zip(v.chunks_exact_mut(cw)).enumerate() {
                let line = std::slice::from_raw_parts(uv.offset(row as isize * stride), cw * 2);
                for ((pair, u), v) in line.as_chunks::<2>().0.iter().zip(u).zip(v) {
                    *u = pair[0];
                    *v = pair[1];
                }
            }
        } else {
            // Anything else (a 4:2:2 or 4:4:4 JPEG) goes through swscale
            self.sws = ff::sws_getCachedContext(self.sws, w as i32, h as i32, std::mem::transmute::<i32, ff::AVPixelFormat>(fmt), w as i32, h as i32,
                                                ff::AVPixelFormat::AV_PIX_FMT_YUV420P, ff::SwsFlags::SWS_BILINEAR as i32,
                                                ptr::null_mut(), ptr::null_mut(), ptr::null());
            if self.sws.is_null() {
                return Err(Fault::Invalid);
            }
            // swscale's SIMD loops run past each row and the end, so it writes into padded rows and the planes are copied out tight
            let (ys, cs) = (padded_stride(w), padded_stride(cw));
            let base = self.sws_out.reserve(ys * h + 2 * cs * ch);
            let dst = [base, base.add(ys * h), base.add(ys * h + cs * ch), ptr::null_mut()];
            let dst_stride = [ys as i32, cs as i32, cs as i32, 0];
            ff::sws_scale(self.sws, (*src).data.as_ptr() as *const *const u8, (*src).linesize.as_ptr(), 0, h as i32, dst.as_ptr(), dst_stride.as_ptr());
            for (from, pw, ph, stride) in [(0, w, h, ys), (ys * h, cw, ch, cs), (ys * h + cs * ch, cw, ch, cs)] {
                for row in 0..ph {
                    i420.extend_from_slice(self.sws_out.bytes(from + row * stride, pw));
                }
            }
            // swscale takes the J formats' full range as it goes; any other format is left as it was
            use ff::AVPixelFormat::*;
            full_range &= ![AV_PIX_FMT_YUVJ422P, AV_PIX_FMT_YUVJ444P, AV_PIX_FMT_YUVJ440P].iter().any(|&j| j as i32 == fmt);
        }
        let (i420, bgr) = self.converter.standard(i420, w, h, yuv::Encoding { matrix, full_range });
        ff::av_frame_unref(self.frame);
        self.pictures = self.pictures.saturating_add(1);
        let frame = Frame::new(w as u32, h as u32, PixelFormat::I420, i420);
        Ok(match bgr {
            Some(bgr) => frame.with_bgr(bgr),
            None => frame,
        })
    }
}

/// Asks Windows to schedule the D3D11 device FFmpeg made ahead of other programs' graphics work, so a game in front doesn't hold the decoder up.
/// AVHWDeviceContext.hwctx points at an AVD3D11VADeviceContext, whose first field is the ID3D11Device (libavutil/hwcontext_d3d11va.h).
#[cfg(windows)]
unsafe fn raise_gpu_priority(buffer: *mut ff::AVBufferRef) {
    use windows::core::{IUnknown, Interface};
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    static SAID: std::sync::Once = std::sync::Once::new();
    let say = |text: String| SAID.call_once(|| log::info!("{text}"));
    if buffer.is_null() || (*buffer).data.is_null() {
        return say("Couldn't find the graphics device to raise its priority".into());
    }
    let ctx = (*buffer).data as *const ff::AVHWDeviceContext;
    let device = if (*ctx).hwctx.is_null() { ptr::null_mut() } else { *((*ctx).hwctx as *const *mut std::ffi::c_void) };
    let Some(unknown) = IUnknown::from_raw_borrowed(&device) else {
        return say("Couldn't find the graphics device to raise its priority".into());
    };
    // SAFETY: the device is alive as long as FFmpeg's device context is, which outlives this call
    match unknown.cast::<IDXGIDevice>().and_then(|dxgi| dxgi.SetGPUThreadPriority(7)) {
        Ok(()) => say("The decoder's graphics device runs at a raised GPU priority".into()),
        Err(e) => say(format!("Couldn't raise the decoder's GPU priority: {e}")),
    }
}

/// Whether a picture copied off the card is a surface the driver never wrote: all zeros, which no camera gives, since even
/// black has chroma at the middle. A few samples down the first two planes, so it costs nothing.
unsafe fn unwritten(f: *const ff::AVFrame) -> bool {
    let (w, h) = ((*f).width as isize, (*f).height as isize);
    (0..16).all(|i| {
        let (x, y) = (w * i / 16, h * i / 16);
        let luma = *(*f).data[0].offset(y * (*f).linesize[0] as isize + x);
        let chroma = *(*f).data[1].offset(y / 2 * (*f).linesize[1] as isize + x / 2);
        luma == 0 && chroma == 0
    })
}

impl Drop for Codec {
    fn drop(&mut self) {
        // SAFETY: each was allocated in open() and is freed exactly once here (the free functions take null)
        unsafe {
            if !self.parser.is_null() {
                ff::av_parser_close(self.parser);
            }
            ff::av_packet_free(&mut self.packet);
            ff::av_frame_free(&mut self.frame);
            ff::av_frame_free(&mut self.sw_frame);
            ff::av_frame_free(&mut self.held);
            ff::avcodec_free_context(&mut self.ctx);
            if !self.hw_device.is_null() {
                ff::av_buffer_unref(&mut self.hw_device);
            }
            ff::sws_freeContext(self.sws);
        }
    }
}

/// A PNG, JPEG, WebP or BMP file as RGBA, in the colours a picture viewer shows (a JPEG's full range kept).
pub fn decode_picture(bytes: &[u8]) -> Result<Frame, String> {
    let id = match kind(bytes) {
        Some(Kind::Png) => ff::AVCodecID::AV_CODEC_ID_PNG,
        Some(Kind::Jpeg) => ff::AVCodecID::AV_CODEC_ID_MJPEG,
        Some(Kind::Webp) => ff::AVCodecID::AV_CODEC_ID_WEBP,
        Some(Kind::Bmp) => ff::AVCodecID::AV_CODEC_ID_BMP,
        None => return Err("not a picture Telescope reads".into()),
    };
    // The decoders read a few bytes past the end
    let mut data = Vec::new();
    padded(&mut data, bytes);
    // SAFETY: every pointer is checked before use and freed on each way out; the packet points at data for the length of send_packet
    unsafe {
        let codec = ff::avcodec_find_decoder(id);
        if codec.is_null() {
            return Err("this FFmpeg can't read that kind of picture".into());
        }
        let mut ctx = ff::avcodec_alloc_context3(codec);
        let mut packet = ff::av_packet_alloc();
        let mut frame = ff::av_frame_alloc();
        let mut sws: *mut ff::SwsContext = ptr::null_mut();
        let result = (|| {
            if ctx.is_null() || packet.is_null() || frame.is_null() {
                return Err("out of memory".to_string());
            }
            let r = ff::avcodec_open2(ctx, codec, ptr::null_mut());
            if r < 0 {
                return Err(err_text(r));
            }
            (*packet).data = data.as_mut_ptr();
            (*packet).size = bytes.len() as i32;
            let mut r = ff::avcodec_send_packet(ctx, packet);
            (*packet).data = ptr::null_mut();
            (*packet).size = 0;
            if r >= 0 {
                ff::avcodec_send_packet(ctx, ptr::null());
                r = ff::avcodec_receive_frame(ctx, frame);
            }
            if r < 0 {
                return Err(err_text(r));
            }
            let (w, h) = ((*frame).width, (*frame).height);
            if w <= 0 || h <= 0 {
                return Err("an empty picture".into());
            }
            sws = ff::sws_getCachedContext(sws, w, h, std::mem::transmute::<i32, ff::AVPixelFormat>((*frame).format), w, h, ff::AVPixelFormat::AV_PIX_FMT_RGBA,
                                           ff::SwsFlags::SWS_BILINEAR as i32, ptr::null_mut(), ptr::null_mut(), ptr::null());
            if sws.is_null() {
                return Err("can't convert that picture".into());
            }
            // Written to a padded buffer, since swscale's SIMD loops run past the end of a tight one
            let (row, stride) = ((w * 4) as usize, padded_stride((w * 4) as usize));
            let mut padded = Padded::default();
            let out = padded.reserve(stride * h as usize);
            let dst = [out, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
            let dst_stride = [stride as i32, 0, 0, 0];
            ff::sws_scale(sws, (*frame).data.as_ptr() as *const *const u8, (*frame).linesize.as_ptr(), 0, h, dst.as_ptr(), dst_stride.as_ptr());
            let mut rgba = vec![0u8; row * h as usize];
            for (i, line) in rgba.chunks_exact_mut(row).enumerate() {
                line.copy_from_slice(padded.bytes(i * stride, row));
            }
            Ok(oriented(rgba, w as usize, h as usize, exif_orientation(bytes)))
        })();
        ff::sws_freeContext(sws);
        ff::av_frame_free(&mut frame);
        ff::av_packet_free(&mut packet);
        ff::avcodec_free_context(&mut ctx);
        result
    }
}

/// Alignment and tail swscale's SIMD loops are given: they work in whole vectors, touching bytes past a row's end.
const SIMD_ALIGN: usize = 64;
const SIMD_TAIL: usize = 64;

/// A row length rounded up to what swscale's SIMD loops stride by.
pub(crate) fn padded_stride(bytes: usize) -> usize {
    bytes.next_multiple_of(SIMD_ALIGN)
}

/// A buffer swscale can run on: aligned, rows padded to a stride, a spare tail after the last row. Reused between calls.
#[derive(Default)]
pub(crate) struct Padded {
    buf: Vec<u8>,
    off: usize,
}

/// Makes buf big enough for len bytes swscale can run on; where they start in it comes back.
pub(crate) fn aligned_start(buf: &mut Vec<u8>, len: usize) -> usize {
    let need = len + SIMD_TAIL + SIMD_ALIGN;
    if buf.len() < need {
        *buf = vec![0; need];
    }
    buf.as_ptr().align_offset(SIMD_ALIGN)
}

impl Padded {
    /// An aligned pointer to len usable bytes with SIMD_TAIL more behind them.
    pub(crate) fn reserve(&mut self, len: usize) -> *mut u8 {
        self.off = aligned_start(&mut self.buf, len);
        // SAFETY: off is under SIMD_ALIGN and buf holds len + SIMD_TAIL bytes after it
        unsafe { self.buf.as_mut_ptr().add(self.off) }
    }

    pub(crate) fn bytes(&self, from: usize, len: usize) -> &[u8] {
        &self.buf[self.off + from..self.off + from + len]
    }

    /// Copies packed I420 planes in with padded rows; the pointers and strides swscale takes come back.
    pub(crate) fn load_i420(&mut self, w: usize, h: usize, planes: (&[u8], &[u8], &[u8])) -> ([*const u8; 4], [i32; 4]) {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        assert!(planes.0.len() >= w * h && planes.1.len() >= cw * ch && planes.2.len() >= cw * ch, "an I420 picture shorter than its size");
        let (ys, cs) = (padded_stride(w), padded_stride(cw));
        let base = self.reserve(ys * h + 2 * cs * ch);
        let mut ptrs = [ptr::null(); 4];
        let mut at = 0;
        for (i, (plane, pw, ph, stride)) in [(planes.0, w, h, ys), (planes.1, cw, ch, cs), (planes.2, cw, ch, cs)].into_iter().enumerate() {
            // SAFETY: the planes sit one after another inside the len reserved above
            unsafe {
                ptrs[i] = base.add(at);
                for row in 0..ph {
                    ptr::copy_nonoverlapping(plane.as_ptr().add(row * pw), base.add(at + row * stride), pw);
                }
            }
            at += stride * ph;
        }
        (ptrs, [ys as i32, cs as i32, cs as i32, 0])
    }
}

/// I420 to RGB on the processor, scaled to the size the window shows it at: the preview when there's no graphics card.
pub struct CpuScaler {
    sws: *mut ff::SwsContext,
    src: Padded,
    dst: Padded,
}

// SAFETY: used from one thread at a time
unsafe impl Send for CpuScaler {}

impl Default for CpuScaler {
    fn default() -> Self {
        CpuScaler { sws: ptr::null_mut(), src: Padded::default(), dst: Padded::default() }
    }
}

impl CpuScaler {
    pub fn rgb(&mut self, frame: &Frame, out_w: u32, out_h: u32) -> Vec<u8> {
        let mut rgb = vec![0u8; (out_w * out_h * 3) as usize];
        let Some(planes) = frame.i420_planes() else { return rgb };
        let (w, h) = (frame.width as i32, frame.height as i32);
        let (row, stride) = ((out_w * 3) as usize, padded_stride((out_w * 3) as usize));
        // swscale runs on padded copies of both sides, then the picture is copied out tight
        let (src, src_stride) = self.src.load_i420(w as usize, h as usize, planes);
        let out = self.dst.reserve(stride * out_h as usize);
        // SAFETY: src and out are padded buffers laid out as the strides say, with swscale's SIMD tail behind them
        unsafe {
            self.sws = ff::sws_getCachedContext(self.sws, w, h, ff::AVPixelFormat::AV_PIX_FMT_YUV420P, out_w as i32, out_h as i32,
                                                ff::AVPixelFormat::AV_PIX_FMT_RGB24, ff::SwsFlags::SWS_BILINEAR as i32,
                                                ptr::null_mut(), ptr::null_mut(), ptr::null());
            if self.sws.is_null() {
                return rgb;
            }
            let dst = [out, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
            let dst_stride = [stride as i32, 0, 0, 0];
            ff::sws_scale(self.sws, src.as_ptr(), src_stride.as_ptr(), 0, h, dst.as_ptr(), dst_stride.as_ptr());
        }
        for (i, line) in rgb.chunks_exact_mut(row).enumerate() {
            line.copy_from_slice(self.dst.bytes(i * stride, row));
        }
        rgb
    }
}

impl Drop for CpuScaler {
    fn drop(&mut self) {
        // SAFETY: made by sws_getCachedContext, freed once
        unsafe { ff::sws_freeContext(self.sws) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_reaches_ffmpeg_with_a_zeroed_padding_tail() {
        let mut buf = vec![7; 200];
        let out = padded(&mut buf, &[1, 2, 3]);
        assert_eq!(out.len(), 3 + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize);
        assert_eq!(&out[..3], &[1, 2, 3]);
        assert!(out[3..].iter().all(|&b| b == 0));
    }
}

#[cfg(test)]
mod scaler_tests {
    use super::*;
    use crate::guard_alloc::overran;

    /// A flat colour picture: Y, U and V all the same across it.
    fn flat(w: u32, h: u32, yuv: (u8, u8, u8)) -> Frame {
        let c = (w.div_ceil(2) * h.div_ceil(2)) as usize;
        Frame::new(w, h, PixelFormat::I420, [vec![yuv.0; (w * h) as usize], vec![yuv.1; c], vec![yuv.2; c]].concat())
    }

    #[test]
    fn scaling_stays_inside_its_buffers_at_awkward_widths() {
        // Y 180, U 100, V 160 is a salmon pink in BT.601 limited range: R 242, G 176, B 134 give or take
        // The size of the picture and the size it's drawn at: unscaled, shrunk and stretched
        for (w, h, ow, oh) in [(638u32, 48u32, 638u32, 48u32), (637, 47, 637, 47), (641, 3, 641, 3), (33, 17, 33, 17), (638, 48, 321, 25), (99, 31, 203, 61)] {
            let size = (ow * oh * 3) as usize;
            let mut scaler = CpuScaler::default();
            let rgb = scaler.rgb(&flat(w, h, (180, 100, 160)), ow, oh);
            assert_eq!(rgb.len(), size);
            for px in rgb.as_chunks::<3>().0 {
                let want = [242, 175, 134];
                assert!(px.iter().zip(want).all(|(&a, b)| (a as i32 - b).abs() <= 3), "{w}x{h} to {ow}x{oh} came out {px:?}");
            }
            drop(rgb);
            drop(scaler);
            assert!(!overran(size), "{w}x{h} to {ow}x{oh}: swscale wrote past the end of the RGB buffer");
        }
    }
}

/// Expected levels are what Python's steps give a flat picture: PyAV to BGR with the stream's matrix and range, then OpenCV's I420.
#[cfg(test)]
mod colour_tests {
    use super::*;
    use crate::testenc;

    fn level(f: &Frame) -> (u8, u8, u8) {
        let (y, u, v) = f.i420_planes().unwrap();
        // Every pixel of a flat picture is the same
        assert!(y.iter().all(|&p| p == y[0]) && u.iter().all(|&p| p == u[0]) && v.iter().all(|&p| p == v[0]));
        (y[0], u[0], v[0])
    }

    fn near(got: (u8, u8, u8), want: (u8, u8, u8)) {
        let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
        assert!(d(got.0, want.0) <= 2 && d(got.1, want.1) <= 2 && d(got.2, want.2) <= 2, "decoded to {got:?}, Python gives {want:?}");
    }

    fn h264(yuv: (u8, u8, u8), matrix: &str, range: &str) -> Option<Frame> {
        let packets = testenc::h264_yuv(yuv.0, yuv.1, yuv.2, 64, 48, matrix, range)?;
        let mut codec = Codec::h264(None).unwrap();
        let mut out = None;
        for p in &packets {
            codec.parse(p, |c, unit| {
                c.decode(unit, |f| out = Some(f)).unwrap();
            });
        }
        Some(out.expect("the stream decodes to a picture"))
    }

    fn jpeg(yuv: (u8, u8, u8)) -> Frame {
        let mut codec = Codec::mjpeg().unwrap();
        let mut out = None;
        codec.decode(&testenc::jpeg_yuv(yuv.0, yuv.1, yuv.2, 64, 48), |f| out = Some(f)).unwrap();
        out.unwrap()
    }

    #[test]
    fn a_full_range_jpeg_keeps_its_blacks_and_whites() {
        // Grey 16 and 235 are real greys in a JPEG; Python's picture has them at 30 and 218 once it is limited range
        near(level(&jpeg((16, 128, 128))), (30, 128, 128));
        near(level(&jpeg((235, 128, 128))), (218, 128, 128));
        near(level(&jpeg((0, 128, 128))), (16, 128, 128));
        near(level(&jpeg((255, 128, 128))), (235, 128, 128));
        near(level(&jpeg((81, 90, 240))), (85, 94, 226));
    }

    #[test]
    fn a_bt709_stream_comes_out_the_way_python_converts_it() {
        // Y 81, U 90, V 240 is BGR [0, 23, 255] in BT.709, where BT.601 would give [0, 0, 253]
        let Some(f) = h264((81, 90, 240), "bt709", "tv") else { return };
        near(level(&f), (93, 84, 231));
    }

    #[test]
    fn a_full_range_stream_is_converted_with_or_without_a_matrix() {
        let Some(f) = h264((81, 90, 240), "bt470bg", "pc") else { return };
        near(level(&f), (85, 94, 226));
        let Some(f) = h264((81, 90, 240), "bt709", "pc") else { return };
        near(level(&f), (100, 84, 226));
    }

    #[test]
    fn bt2020_and_the_other_named_matrices_are_honoured_too() {
        let Some(f) = h264((81, 90, 240), "bt2020nc", "tv") else { return };
        near(level(&f), (86, 88, 237));
    }

    #[test]
    fn a_plain_bt601_limited_stream_is_left_as_it_is() {
        let Some(f) = h264((81, 90, 240), "bt470bg", "tv") else { return };
        near(level(&f), (81, 90, 240));
    }
}

#[cfg(test)]
mod picture_tests {
    use super::*;
    use crate::picture::tests::ORIENTED;

    /// A 2 x 1 BMP: a red pixel then a blue one.
    fn bmp() -> Vec<u8> {
        let mut b = b"BM".to_vec();
        b.extend_from_slice(&(54u32 + 8).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&2i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        // BGR rows pad to four bytes
        b.extend_from_slice(&[0, 0, 255, 255, 0, 0, 0, 0]);
        b
    }

    #[test]
    fn a_bmp_reads_as_rgba() {
        let f = decode_picture(&bmp()).unwrap();
        assert_eq!((f.width, f.height, f.format), (2, 1, PixelFormat::Rgba));
        assert_eq!(&f.data[..8], &[255, 0, 0, 255, 0, 0, 255, 255]);
    }

    // A 16 x 8 JPEG with orientation 6, which cv2.imdecode makes 8 x 16
    const TURNED_JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xff, 0xe1, 0x00, 0x22, 0x45, 0x78, 0x69, 0x66, 0x00, 0x00, 0x4d, 0x4d, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08, 0x00, 0x01, 0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x03, 0x02, 0x02, 0x03, 0x03, 0x03, 0x03, 0x04, 0x03, 0x03, 0x04, 0x05, 0x08, 0x05, 0x05, 0x04, 0x04, 0x05, 0x0a, 0x07, 0x07, 0x06, 0x08, 0x0c, 0x0a, 0x0c, 0x0c, 0x0b, 0x0a, 0x0b, 0x0b, 0x0d, 0x0e, 0x12, 0x10, 0x0d, 0x0e, 0x11, 0x0e, 0x0b, 0x0b, 0x10, 0x16, 0x10, 0x11, 0x13, 0x14, 0x15, 0x15, 0x15, 0x0c, 0x0f, 0x17, 0x18, 0x16, 0x14, 0x18, 0x12, 0x14, 0x15, 0x14, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x03, 0x04, 0x04, 0x05, 0x04, 0x05, 0x09, 0x05, 0x05, 0x09, 0x14, 0x0d, 0x0b, 0x0d, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x08, 0x00, 0x10, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xff, 0xc4, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7d, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xc4, 0x00, 0x1f, 0x01, 0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x11, 0x00, 0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77, 0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0xf1, 0x4a, 0x28, 0xa2, 0xbf, 0x37, 0x3f, 0xb5, 0x4f, 0xff, 0xd9];

    #[test]
    fn a_picture_turns_by_its_exif_orientation_as_opencv_does() {
        for (o, file, w, h, want) in ORIENTED {
            let f = decode_picture(file).unwrap();
            assert_eq!((exif_orientation(file), f.width, f.height), (*o, *w, *h), "orientation {o}");
            assert_eq!(&f.data[..], *want, "orientation {o}");
        }
    }

    #[test]
    fn a_turned_jpeg_comes_back_on_its_side() {
        let f = decode_picture(TURNED_JPEG).unwrap();
        assert_eq!((f.width, f.height), (8, 16));
    }

    #[test]
    fn a_picture_with_no_exif_is_left_as_it_is() {
        assert_eq!(exif_orientation(&bmp()), 1);
        assert_eq!(exif_orientation(&[0xff, 0xd8, 0xff, 0xda, 0, 2]), 1);
    }

    /// A w x 3 BMP of one colour.
    fn wide_bmp(w: usize, bgr: [u8; 3]) -> Vec<u8> {
        let row = (w * 3).next_multiple_of(4);
        let mut b = b"BM".to_vec();
        b.extend_from_slice(&((54 + row * 3) as u32).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&(w as i32).to_le_bytes());
        b.extend_from_slice(&3i32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        for _ in 0..3 {
            let mut line: Vec<u8> = bgr.iter().copied().cycle().take(w * 3).collect();
            line.resize(row, 0);
            b.extend_from_slice(&line);
        }
        b
    }

    #[test]
    fn a_wide_picture_reads_without_writing_past_its_buffer() {
        for w in [637usize, 638, 641] {
            let f = decode_picture(&wide_bmp(w, [10, 20, 30])).unwrap();
            assert_eq!((f.width as usize, f.height), (w, 3));
            assert!(f.data.as_chunks::<4>().0.iter().all(|p| *p == [30, 20, 10, 255]));
            let size = f.data.len();
            drop(f);
            assert!(!crate::guard_alloc::overran(size), "{w}: swscale wrote past the end of the RGBA buffer");
        }
    }

    #[test]
    fn a_422_jpeg_converts_without_writing_past_its_buffer() {
        for w in [637usize, 638, 641] {
            let mut codec = Codec::mjpeg().unwrap();
            let mut out = None;
            codec.decode(&crate::testenc::jpeg_422(100, w as u32, 3), |f| out = Some(f)).unwrap();
            let f = out.unwrap();
            assert_eq!((f.width as usize, f.height), (w, 3));
            let size = w * 3 + 2 * w.div_ceil(2) * 2;
            assert_eq!(f.data.len(), size);
            drop(f);
            assert!(!crate::guard_alloc::overran(size), "{w}: swscale wrote past the end of the I420 buffer");
        }
    }

    #[test]
    fn a_codec_without_a_parser_takes_no_stream_bytes() {
        let mut codec = Codec::mjpeg().unwrap();
        codec.parse(b"\x00\x00\x00\x01abc", |_, _| panic!("no parser, no units"));
    }

    #[test]
    fn something_else_is_refused() {
        assert!(decode_picture(b"hello there, not a picture").is_err());
        assert!(decode_picture(&bmp()[..30]).is_err());
    }
}

/// Timed runs on recorded-phone-like clips. TELESCOPE_BENCH_CLIPS is a folder of Annex-B .h264 files with an access unit
/// delimiter in front of each picture (made with ffmpeg, libx264 high profile, no B-frames, a keyframe a second).
#[cfg(test)]
pub(crate) mod decode_bench {
    use super::*;
    use std::hash::{Hash, Hasher};
    use std::time::Instant;

    pub(crate) fn clip_dir() -> std::path::PathBuf {
        std::env::var_os("TELESCOPE_BENCH_CLIPS").map(Into::into)
            .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/telescope-scratch/decode-clips"))
    }

    /// The file cut into access units at each delimiter (NAL type 9).
    pub(crate) fn packets(name: &str) -> Option<Vec<Vec<u8>>> {
        let data = std::fs::read(clip_dir().join(name)).ok()?;
        let mut starts = Vec::new();
        for i in 0..data.len().saturating_sub(5) {
            if data[i..i + 3] == [0, 0, 1] && data[i + 3] & 0x1f == 9 {
                starts.push(if i > 0 && data[i - 1] == 0 { i - 1 } else { i });
            }
        }
        starts.push(data.len());
        Some(starts.windows(2).map(|w| data[w[0]..w[1]].to_vec()).collect())
    }

    pub(crate) fn digest(frames: &[Frame]) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for f in frames {
            (f.width, f.height).hash(&mut h);
            f.data.hash(&mut h);
        }
        h.finish()
    }

    pub(crate) struct Run {
        pub per_packet_ms: Vec<f64>,
        pub frames: Vec<Frame>,
        pub lag: Vec<usize>,
    }

    pub(crate) fn run(codec: &mut Codec, packets: &[Vec<u8>]) -> Run {
        let mut out = Run { per_packet_ms: Vec::new(), frames: Vec::new(), lag: Vec::new() };
        for (i, p) in packets.iter().enumerate() {
            let began = Instant::now();
            codec.parse(p, |c, unit| {
                c.decode(unit, |f| {
                    out.lag.push(i.saturating_sub(out.frames.len()));
                    out.frames.push(f);
                }).unwrap();
            });
            out.per_packet_ms.push(began.elapsed().as_secs_f64() * 1000.0);
        }
        out
    }

    pub(crate) fn pct(v: &[f64], p: f64) -> f64 {
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s[((s.len() - 1) as f64 * p) as usize]
    }

    /// What each picture's copy off a VA-API card takes, asking for I420 and for the card's own NV12.
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_the_copy_off_the_card_costs
    #[test]
    #[ignore = "a measurement, needs the clips and a VA-API card"]
    fn what_the_copy_off_the_card_costs() {
        for name in ["p1080.h264", "p4k.h264"] {
            let Some(packets) = packets(name) else { continue };
            for i420 in [true, false] {
                let mut codec = Codec::h264(Some(Hw::Vaapi)).expect("VA-API");
                codec.card_gives_i420 = i420;
                codec.copy_ns = Some(Vec::new());
                let r = run(&mut codec, &packets);
                let times = codec.copy_ns.take().unwrap();
                let times = &times[30..];
                let ms = |mut v: Vec<u64>| {
                    v.sort();
                    v[v.len() / 2] as f64 / 1e6
                };
                println!("{name:11} as {}: copy {:.2} ms waiting on the decoder, {:.2} ms with it done; {} pictures", if i420 { "I420" } else { "NV12" },
                         ms(times.iter().map(|t| t.0).collect()), ms(times.iter().map(|t| t.1).collect()), r.frames.len());
            }
        }
    }

    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture software_thread_counts
    #[test]
    #[ignore = "a measurement, needs the clips"]
    fn software_thread_counts() {
        for name in ["p1080.h264", "p1080s4.h264", "p4k.h264", "p4ks4.h264"] {
            let Some(packets) = packets(name) else {
                eprintln!("no clip {name}");
                continue;
            };
            let mut reference = None;
            for (count, whole_frames) in [(1, false), (2, false), (4, false), (8, false), (0, false), (2, true), (4, true), (8, true), (0, true)] {
                let mut best: Option<(f64, f64, f64, usize, usize, u64, i32)> = None;
                for _ in 0..3 {
                    let mut codec = Codec::h264_threaded(Threads { count, whole_frames }).unwrap();
                    let active = unsafe { ((*codec.ctx).active_thread_type, (*codec.ctx).thread_count) };
                    let began = Instant::now();
                    let r = run(&mut codec, &packets);
                    let total = began.elapsed().as_secs_f64() * 1000.0;
                    let per = total / r.frames.len().max(1) as f64;
                    let maxlag = r.lag.iter().copied().max().unwrap_or(0);
                    let row = (per, pct(&r.per_packet_ms, 0.5), pct(&r.per_packet_ms, 0.99), maxlag, r.frames.len(), digest(&r.frames), active.0);
                    let _ = active.1;
                    if best.map_or(true, |b| row.0 < b.0) {
                        best = Some(row);
                    }
                }
                let b = best.unwrap();
                let same = *reference.get_or_insert(b.5) == b.5;
                println!("{name:13} threads {count} {:6} active type {} | {:5.2} ms/frame, packet p50 {:5.2} p99 {:5.2}, max frame lag {} packets, {} frames, same pixels as 1 thread: {same}",
                         if whole_frames { "frames" } else { "slices" }, b.6, b.0, b.1, b.2, b.3, b.4);
            }
        }
    }
}

/// take_frame on synthetic pictures: a reference built from the plain planes, and timings by source format and range.
#[cfg(test)]
mod extract_tests {
    use super::*;
    use std::time::Instant;

    /// Gives the codec a decoded picture of this format, full of a pattern that differs per plane, padding included.
    unsafe fn load(codec: &mut Codec, fmt: ff::AVPixelFormat, w: usize, h: usize, matrix: i32, full: bool, seed: u32) {
        let f = codec.frame;
        ff::av_frame_unref(f);
        (*f).format = fmt as i32;
        (*f).width = w as i32;
        (*f).height = h as i32;
        assert!(ff::av_frame_get_buffer(f, 0) >= 0);
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        let (ch, nv12) = (h.div_ceil(2), fmt == ff::AVPixelFormat::AV_PIX_FMT_NV12);
        for (plane, rows) in [(0, h), (1, ch), (2, ch)] {
            if plane == 2 && nv12 || (*f).data[plane].is_null() {
                continue;
            }
            let bytes = std::slice::from_raw_parts_mut((*f).data[plane], (*f).linesize[plane] as usize * rows);
            for b in bytes {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = (x >> 11) as u8;
            }
        }
        ptr::addr_of_mut!((*f).colorspace).cast::<i32>().write(matrix);
        let range = if full { ff::AVColorRange::AVCOL_RANGE_JPEG } else { ff::AVColorRange::AVCOL_RANGE_MPEG };
        ptr::addr_of_mut!((*f).color_range).cast::<i32>().write(range as i32);
    }

    /// The picture's planes as packed I420 straight from the definition: rows without padding, NV12 chroma split in two.
    unsafe fn expected(codec: &Codec, w: usize, h: usize) -> Vec<u8> {
        let f = codec.frame;
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let row = |plane: usize, y: usize, n: usize| std::slice::from_raw_parts((*f).data[plane].add(y * (*f).linesize[plane] as usize), n);
        let mut out: Vec<u8> = (0..h).flat_map(|y| row(0, y, w).to_vec()).collect();
        if (*f).format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
            out.extend((0..ch).flat_map(|y| row(1, y, cw * 2).iter().step_by(2).copied().collect::<Vec<_>>()));
            out.extend((0..ch).flat_map(|y| row(1, y, cw * 2).iter().skip(1).step_by(2).copied().collect::<Vec<_>>()));
        } else {
            out.extend((0..ch).flat_map(|y| row(1, y, cw).to_vec()));
            out.extend((0..ch).flat_map(|y| row(2, y, cw).to_vec()));
        }
        out
    }

    const CASES: [(ff::AVPixelFormat, i32, bool, &str); 6] = [
        (ff::AVPixelFormat::AV_PIX_FMT_YUV420P, 5, false, "I420 limited"),
        (ff::AVPixelFormat::AV_PIX_FMT_YUV420P, 5, true, "I420 full"),
        (ff::AVPixelFormat::AV_PIX_FMT_NV12, 5, false, "NV12 limited"),
        (ff::AVPixelFormat::AV_PIX_FMT_NV12, 5, true, "NV12 full"),
        (ff::AVPixelFormat::AV_PIX_FMT_YUV420P, 1, false, "I420 BT.709"),
        (ff::AVPixelFormat::AV_PIX_FMT_NV12, 1, true, "NV12 BT.709 full"),
    ];

    /// What the old take_frame gave: the planes, then the range squeeze or the BGR round trip.
    fn reference(planes: Vec<u8>, w: usize, h: usize, matrix: i32, full: bool) -> Vec<u8> {
        yuv::Converter::default().standard(planes, w, h, yuv::Encoding { matrix, full_range: full }).0
    }

    #[test]
    fn every_source_gives_the_same_bytes_as_its_definition() {
        let mut codec = Codec::h264(None).unwrap();
        // Odd sizes, and widths with padding on the end of each row
        for (w, h) in [(64, 48), (37, 21), (2, 2), (1, 1), (130, 7)] {
            for (n, (fmt, matrix, full, name)) in CASES.into_iter().enumerate() {
                unsafe {
                    load(&mut codec, fmt, w, h, matrix, full, n as u32 + 1);
                    let want = reference(expected(&codec, w, h), w, h, matrix, full);
                    let got = codec.take_frame().unwrap();
                    assert_eq!((got.width as usize, got.height as usize), (w, h), "{name} {w}x{h}");
                    assert!(*got.data == want, "{name} {w}x{h} differs from its definition");
                }
            }
        }
    }

    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_extracting_costs
    #[test]
    #[ignore = "a measurement"]
    fn what_extracting_costs() {
        let mut codec = Codec::h264(None).unwrap();
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            println!("{w}x{h}");
            for (n, (fmt, matrix, full, name)) in CASES.into_iter().enumerate() {
                let mut times = Vec::new();
                let mut hash = 0u64;
                for round in 0..25 {
                    unsafe {
                        load(&mut codec, fmt, w, h, matrix, full, n as u32 + 1);
                        let began = Instant::now();
                        let frame = codec.take_frame().unwrap();
                        times.push(began.elapsed().as_nanos() as u64);
                        if round == 0 {
                            hash = frame.data.iter().fold(1469598103934665603u64, |a, &b| (a ^ b as u64).wrapping_mul(1099511628211));
                        }
                    }
                }
                times.sort();
                println!("  {name:<18} median {:6.2} ms, best {:6.2} ms, hash {hash:016x}", times[12] as f64 / 1e6, times[0] as f64 / 1e6);
            }
        }
    }
}
