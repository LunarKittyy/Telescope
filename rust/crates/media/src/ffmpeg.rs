//! libavcodec underneath the decoders: one codec context and what it needs, and decoded pictures copied out packed.

use ffmpeg_sys_next as ff;
use std::ptr;
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
    pub fn parse(&mut self, mut data: &[u8], mut unit: impl FnMut(&mut Codec, &[u8])) {
        // SAFETY: the parser reads data within its length and hands back a pointer into its own buffer, valid until the
        // next parse call; unit copies what it needs before that
        unsafe {
            while !data.is_empty() {
                let mut out: *mut u8 = ptr::null_mut();
                let mut out_len: i32 = 0;
                let used = ff::av_parser_parse2(self.parser, self.ctx, &mut out, &mut out_len, data.as_ptr(), data.len().min(i32::MAX as usize) as i32,
                                                ff::AV_NOPTS_VALUE, ff::AV_NOPTS_VALUE, 0);
                if used < 0 {
                    break;
                }
                data = &data[used as usize..];
                if out_len > 0 {
                    let piece = std::slice::from_raw_parts(out, out_len as usize).to_vec();
                    unit(self, &piece);
                }
            }
        }
    }

    /// Decodes one access unit (or one whole JPEG); every frame it finishes goes to done.
    pub fn decode(&mut self, unit: &[u8], mut done: impl FnMut(Frame)) -> Result<u32, Fault> {
        let mut count = 0;
        // SAFETY: the packet points at unit for the length of send_packet, which copies it (the packet isn't refcounted)
        unsafe {
            (*self.packet).data = unit.as_ptr() as *mut u8;
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
            let r = ff::av_hwframe_transfer_data(self.sw_frame, src, 0);
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

/// I420 to RGB on the processor, scaled to the size the window shows it at: the preview when there's no graphics card.
pub struct CpuScaler {
    sws: *mut ff::SwsContext,
}

// SAFETY: used from one thread at a time
unsafe impl Send for CpuScaler {}

impl Default for CpuScaler {
    fn default() -> Self {
        CpuScaler { sws: ptr::null_mut() }
    }
}

impl CpuScaler {
    pub fn rgb(&mut self, frame: &Frame, out_w: u32, out_h: u32) -> Vec<u8> {
        let mut rgb = vec![0u8; (out_w * out_h * 3) as usize];
        let Some((y, u, v)) = frame.i420_planes() else { return rgb };
        let (w, h) = (frame.width as i32, frame.height as i32);
        let cw = (w + 1) / 2;
        // SAFETY: plane pointers and strides describe frame.data exactly; rgb holds out_w * out_h * 3 bytes
        unsafe {
            self.sws = ff::sws_getCachedContext(self.sws, w, h, ff::AVPixelFormat::AV_PIX_FMT_YUV420P, out_w as i32, out_h as i32,
                                                ff::AVPixelFormat::AV_PIX_FMT_RGB24, ff::SwsFlags::SWS_BILINEAR as i32,
                                                ptr::null_mut(), ptr::null_mut(), ptr::null());
            if self.sws.is_null() {
                return rgb;
            }
            let src = [y.as_ptr(), u.as_ptr(), v.as_ptr(), ptr::null()];
            let src_stride = [w, cw, cw, 0];
            let dst = [rgb.as_mut_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
            let dst_stride = [(out_w * 3) as i32, 0, 0, 0];
            ff::sws_scale(self.sws, src.as_ptr(), src_stride.as_ptr(), 0, h, dst.as_ptr(), dst_stride.as_ptr());
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
