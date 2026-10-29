//! libavcodec underneath the decoders: one codec context and what it needs, and decoded pictures copied out packed.

use ffmpeg_sys_next as ff;
use std::ptr;
use crate::yuv;
use telescope_core::frame::{Frame, PixelFormat};

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

/// One libavcodec decoder with its parser (for H.264) and buffers.
pub struct Codec {
    ctx: *mut ff::AVCodecContext,
    parser: *mut ff::AVCodecParserContext,
    packet: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
    sw_frame: *mut ff::AVFrame,
    hw_device: *mut ff::AVBufferRef,
    sws: *mut ff::SwsContext,
    pub hw: Option<Hw>,
    // A JPEG decoder: JFIF pictures are always full range BT.601, whatever the frame says (OpenCV decodes them so)
    jpeg: bool,
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

    fn open(id: ff::AVCodecID, hw: Option<Hw>, parse: bool) -> Result<Codec, String> {
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
                hw_device: ptr::null_mut(),
                sws: ptr::null_mut(),
                hw,
                jpeg: id == ff::AVCodecID::AV_CODEC_ID_MJPEG,
                parse_buf: Vec::new(),
                unit_buf: Vec::new(),
            };
            if c.ctx.is_null() || c.packet.is_null() || c.frame.is_null() || c.sw_frame.is_null() || (parse && c.parser.is_null()) {
                return Err("out of memory opening a decoder".into());
            }
            // Show each frame as soon as it's decoded instead of holding some back for reordering (h264_reader.py)
            (*c.ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
            match hw {
                Some(h) => {
                    let r = ff::av_hwdevice_ctx_create(&mut c.hw_device, h.device_type(), ptr::null(), ptr::null_mut(), 0);
                    if r < 0 {
                        return Err(format!("{} didn't open: {}", h.name(), err_text(r)));
                    }
                    (*c.ctx).hw_device_ctx = ff::av_buffer_ref(c.hw_device);
                    (*c.ctx).opaque = h.pixel_format() as i32 as usize as *mut _;
                    (*c.ctx).get_format = Some(pick_hw);
                }
                None if parse => (*c.ctx).thread_count = 2,
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
        let mut count = 0;
        let input = padded(&mut self.unit_buf, unit).as_ptr();
        // SAFETY: the packet points at a padded copy of unit for the length of send_packet, which copies it (the packet
        // isn't refcounted)
        unsafe {
            (*self.packet).data = input as *mut u8;
            (*self.packet).size = unit.len() as i32;
            let r = ff::avcodec_send_packet(self.ctx, self.packet);
            (*self.packet).data = ptr::null_mut();
            (*self.packet).size = 0;
            if r == ff::AVERROR_INVALIDDATA {
                return Err(Fault::Invalid);
            }
            if r < 0 && r != ff::AVERROR(libc::EAGAIN) {
                return Err(Fault::Broken(err_text(r)));
            }
            loop {
                let r = ff::avcodec_receive_frame(self.ctx, self.frame);
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
                done(self.take_frame()?);
            }
        }
        Ok(count)
    }

    /// The decoded picture as one packed I420 frame (NV12 from the card is split into U and V).
    unsafe fn take_frame(&mut self) -> Result<Frame, Fault> {
        let mut src = self.frame;
        if Hw::from_pixel_format((*src).format).is_some() {
            ff::av_frame_unref(self.sw_frame);
            // Ask for I420 so the driver converts while it copies; one that only gives NV12 is split below
            (*self.sw_frame).format = ff::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
            let mut r = ff::av_hwframe_transfer_data(self.sw_frame, src, 0);
            if r < 0 {
                ff::av_frame_unref(self.sw_frame);
                r = ff::av_hwframe_transfer_data(self.sw_frame, src, 0);
            }
            if r < 0 {
                return Err(Fault::Broken(format!("couldn't copy the picture off the card: {}", err_text(r))));
            }
            src = self.sw_frame;
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
        // Whether the numbers use the whole 0..255: a JPEG always does, a stream says so in its headers
        let mut full_range = self.jpeg || fmt == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32 || (*src).color_range == ff::AVColorRange::AVCOL_RANGE_JPEG;
        let yuv420 = [ff::AVPixelFormat::AV_PIX_FMT_YUV420P as i32, ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32];
        if yuv420.contains(&fmt) {
            copy_plane(&mut i420, (*src).data[0], (*src).linesize[0], w, h);
            copy_plane(&mut i420, (*src).data[1], (*src).linesize[1], cw, ch);
            copy_plane(&mut i420, (*src).data[2], (*src).linesize[2], cw, ch);
        } else if fmt == ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
            copy_plane(&mut i420, (*src).data[0], (*src).linesize[0], w, h);
            let (uv, stride) = ((*src).data[1], (*src).linesize[1] as isize);
            let mut v = Vec::with_capacity(cw * ch);
            for row in 0..ch {
                let line = std::slice::from_raw_parts(uv.offset(row as isize * stride), cw * 2);
                i420.extend(line.iter().step_by(2));
                v.extend(line.iter().skip(1).step_by(2));
            }
            i420.extend_from_slice(&v);
        } else {
            // Anything else (a 4:2:2 or 4:4:4 JPEG) goes through swscale
            i420.resize(w * h + 2 * cw * ch, 0);
            self.sws = ff::sws_getCachedContext(self.sws, w as i32, h as i32, std::mem::transmute::<i32, ff::AVPixelFormat>(fmt), w as i32, h as i32,
                                                ff::AVPixelFormat::AV_PIX_FMT_YUV420P, ff::SwsFlags::SWS_BILINEAR as i32,
                                                ptr::null_mut(), ptr::null_mut(), ptr::null());
            if self.sws.is_null() {
                return Err(Fault::Invalid);
            }
            let base = i420.as_mut_ptr();
            let dst = [base, base.add(w * h), base.add(w * h + cw * ch), ptr::null_mut()];
            let dst_stride = [w as i32, cw as i32, cw as i32, 0];
            ff::sws_scale(self.sws, (*src).data.as_ptr() as *const *const u8, (*src).linesize.as_ptr(), 0, h as i32, dst.as_ptr(), dst_stride.as_ptr());
            // swscale takes the J formats' full range as it goes; any other format is left as it was
            use ff::AVPixelFormat::*;
            full_range &= ![AV_PIX_FMT_YUVJ422P, AV_PIX_FMT_YUVJ444P, AV_PIX_FMT_YUVJ440P].iter().any(|&j| j as i32 == fmt);
        }
        if full_range {
            yuv::full_to_limited(&mut i420, w, h);
        }
        ff::av_frame_unref(self.frame);
        Ok(Frame::new(w as u32, h as u32, PixelFormat::I420, i420))
    }
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
    let id = if bytes.starts_with(b"\x89PNG") {
        ff::AVCodecID::AV_CODEC_ID_PNG
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        ff::AVCodecID::AV_CODEC_ID_MJPEG
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        ff::AVCodecID::AV_CODEC_ID_WEBP
    } else if bytes.starts_with(b"BM") {
        ff::AVCodecID::AV_CODEC_ID_BMP
    } else {
        return Err("not a picture Telescope reads".into());
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
            Ok(Frame::new(w as u32, h as u32, PixelFormat::Rgba, rgba))
        })();
        ff::sws_freeContext(sws);
        ff::av_frame_free(&mut frame);
        ff::av_packet_free(&mut packet);
        ff::avcodec_free_context(&mut ctx);
        result
    }
}

/// Alignment and tail swscale's SIMD loops are allowed: they work in whole vectors, so they touch bytes past a row's end
/// and past the last row.
const SIMD_ALIGN: usize = 64;
const SIMD_TAIL: usize = 64;

/// A row length rounded up to what swscale's SIMD loops stride by.
fn padded_stride(bytes: usize) -> usize {
    bytes.next_multiple_of(SIMD_ALIGN)
}

/// A buffer swscale can run on: aligned, rows padded to a stride, a spare tail after the last row. Reused between calls.
#[derive(Default)]
struct Padded {
    buf: Vec<u8>,
    off: usize,
}

impl Padded {
    /// An aligned pointer to len usable bytes with SIMD_TAIL more behind them.
    fn reserve(&mut self, len: usize) -> *mut u8 {
        let need = len + SIMD_TAIL + SIMD_ALIGN;
        if self.buf.len() < need {
            self.buf = vec![0; need];
        }
        self.off = self.buf.as_ptr().align_offset(SIMD_ALIGN);
        // SAFETY: off is under SIMD_ALIGN and buf holds len + SIMD_TAIL bytes after it
        unsafe { self.buf.as_mut_ptr().add(self.off) }
    }

    fn bytes(&self, from: usize, len: usize) -> &[u8] {
        &self.buf[self.off + from..self.off + from + len]
    }

    /// Copies packed I420 planes in with padded rows; the pointers and strides swscale takes come back.
    fn load_i420(&mut self, w: usize, h: usize, planes: (&[u8], &[u8], &[u8])) -> ([*const u8; 4], [i32; 4]) {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
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
            for px in rgb.chunks_exact(3) {
                let want = [242, 175, 134];
                assert!(px.iter().zip(want).all(|(&a, b)| (a as i32 - b).abs() <= 3), "{w}x{h} to {ow}x{oh} came out {px:?}");
            }
            drop(rgb);
            drop(scaler);
            assert!(!overran(size), "{w}x{h} to {ow}x{oh}: swscale wrote past the end of the RGB buffer");
        }
    }
}

/// What the Python app ends up with for a flat picture, measured by running its steps: PyAV to BGR with the stream's
/// matrix and range, then OpenCV's BGR-to-I420, which writes BT.601 in the limited range.
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
    fn a_full_range_stream_is_converted() {
        let Some(f) = h264((81, 90, 240), "bt470bg", "pc") else { return };
        near(level(&f), (85, 94, 226));
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
            assert!(f.data.chunks_exact(4).all(|p| p == [30, 20, 10, 255]));
            let size = f.data.len();
            drop(f);
            assert!(!crate::guard_alloc::overran(size), "{w}: swscale wrote past the end of the RGBA buffer");
        }
    }

    #[test]
    fn something_else_is_refused() {
        assert!(decode_picture(b"hello there, not a picture").is_err());
        assert!(decode_picture(&bmp()[..30]).is_err());
    }
}
