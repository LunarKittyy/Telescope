//! H.264 (Annex-B) to I420 with FFmpeg's libavcodec, the same decoder PyAV wraps for the Python app, optionally on
//! the graphics card through VA-API. Frames come out as one packed I420 buffer, the format v4l2loopback takes as is.

use ffmpeg_sys_next as ff;
use std::ptr;

/// One decoded picture, packed I420 (Y plane, then U, then V, no row padding).
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub i420: Vec<u8>,
}

pub struct Decoder {
    ctx: *mut ff::AVCodecContext,
    parser: *mut ff::AVCodecParserContext,
    packet: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
    sw_frame: *mut ff::AVFrame,
    hw_device: *mut ff::AVBufferRef,
    pub hardware: bool,
}

// SAFETY: the decoder is used from one thread at a time; FFmpeg contexts aren't tied to the thread that made them
unsafe impl Send for Decoder {}

unsafe extern "C" fn pick_vaapi(_ctx: *mut ff::AVCodecContext, mut formats: *const ff::AVPixelFormat) -> ff::AVPixelFormat {
    while *formats != ff::AVPixelFormat::AV_PIX_FMT_NONE {
        if *formats == ff::AVPixelFormat::AV_PIX_FMT_VAAPI {
            return *formats;
        }
        formats = formats.add(1);
    }
    ff::AVPixelFormat::AV_PIX_FMT_NONE
}

impl Decoder {
    /// A software decoder, or a VA-API one when `vaapi` and the card opens (falls back to software otherwise).
    pub fn new(vaapi: bool) -> Result<Decoder, String> {
        // SAFETY: plain libavcodec setup; every pointer is checked before use and freed in Drop
        unsafe {
            let codec = ff::avcodec_find_decoder(ff::AVCodecID::AV_CODEC_ID_H264);
            if codec.is_null() {
                return Err("this FFmpeg has no H.264 decoder".into());
            }
            let ctx = ff::avcodec_alloc_context3(codec);
            // Show each frame as soon as it's decoded instead of holding some back for reordering (as h264_reader.py)
            (*ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
            let mut hw_device = ptr::null_mut();
            let mut hardware = false;
            if vaapi
                && ff::av_hwdevice_ctx_create(&mut hw_device, ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI, ptr::null(), ptr::null_mut(), 0) == 0
            {
                (*ctx).hw_device_ctx = ff::av_buffer_ref(hw_device);
                (*ctx).get_format = Some(pick_vaapi);
                hardware = true;
            } else {
                (*ctx).thread_count = 2;
            }
            if ff::avcodec_open2(ctx, codec, ptr::null_mut()) < 0 {
                return Err("couldn't open the H.264 decoder".into());
            }
            Ok(Decoder {
                ctx,
                parser: ff::av_parser_init(ff::AVCodecID::AV_CODEC_ID_H264 as i32),
                packet: ff::av_packet_alloc(),
                frame: ff::av_frame_alloc(),
                sw_frame: ff::av_frame_alloc(),
                hw_device,
                hardware,
            })
        }
    }

    /// Feeds stream bytes; returns the newest frame they completed (older ones are dropped to keep latency down after
    /// a stall, like decode_newest) and how many frames they completed.
    pub fn feed(&mut self, mut data: &[u8]) -> (Option<Frame>, u32) {
        let mut newest = None;
        let mut count = 0;
        // SAFETY: the parser reads `data` within its length and hands back a pointer into its own buffer, valid until
        // the next parse call, which avcodec_send_packet copies from before that
        unsafe {
            while !data.is_empty() {
                let mut out: *mut u8 = ptr::null_mut();
                let mut out_len: i32 = 0;
                let used = ff::av_parser_parse2(self.parser, self.ctx, &mut out, &mut out_len, data.as_ptr(), data.len() as i32,
                                                ff::AV_NOPTS_VALUE, ff::AV_NOPTS_VALUE, 0);
                if used < 0 {
                    break;
                }
                data = &data[used as usize..];
                if out_len == 0 {
                    continue;
                }
                (*self.packet).data = out;
                (*self.packet).size = out_len;
                // A packet before the first keyframe is refused; that's expected when joining mid-stream
                if ff::avcodec_send_packet(self.ctx, self.packet) < 0 {
                    continue;
                }
                while ff::avcodec_receive_frame(self.ctx, self.frame) == 0 {
                    count += 1;
                    newest = self.take_frame();
                }
            }
        }
        (newest, count)
    }

    unsafe fn take_frame(&mut self) -> Option<Frame> {
        let mut src = self.frame;
        if (*src).format == ff::AVPixelFormat::AV_PIX_FMT_VAAPI as i32 {
            ff::av_frame_unref(self.sw_frame);
            if ff::av_hwframe_transfer_data(self.sw_frame, src, 0) < 0 {
                return None;
            }
            src = self.sw_frame;
        }
        let (w, h) = ((*src).width as usize, (*src).height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut i420 = Vec::with_capacity(w * h + 2 * cw * ch);
        let copy_plane = |out: &mut Vec<u8>, data: *const u8, stride: i32, pw: usize, ph: usize| {
            for row in 0..ph {
                out.extend_from_slice(std::slice::from_raw_parts(data.offset(row as isize * stride as isize), pw));
            }
        };
        let fmt = (*src).format;
        copy_plane(&mut i420, (*src).data[0], (*src).linesize[0], w, h);
        if fmt == ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
            // The card hands back NV12 (U and V interleaved); split it into I420's two planes
            let (uv, stride) = ((*src).data[1], (*src).linesize[1] as isize);
            let mut v = Vec::with_capacity(cw * ch);
            for row in 0..ch {
                let line = std::slice::from_raw_parts(uv.offset(row as isize * stride), cw * 2);
                i420.extend(line.iter().step_by(2));
                v.extend(line.iter().skip(1).step_by(2));
            }
            i420.extend_from_slice(&v);
        } else {
            copy_plane(&mut i420, (*src).data[1], (*src).linesize[1], cw, ch);
            copy_plane(&mut i420, (*src).data[2], (*src).linesize[2], cw, ch);
        }
        Some(Frame { width: w as u32, height: h as u32, i420 })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: each was allocated in new() and is freed exactly once here
        unsafe {
            ff::av_parser_close(self.parser);
            ff::av_packet_free(&mut self.packet);
            ff::av_frame_free(&mut self.frame);
            ff::av_frame_free(&mut self.sw_frame);
            ff::avcodec_free_context(&mut self.ctx);
            if !self.hw_device.is_null() {
                ff::av_buffer_unref(&mut self.hw_device);
            }
        }
    }
}

/// I420 to RGB for the preview, scaled to the size the window shows it at.
pub struct PreviewScaler {
    sws: *mut ff::SwsContext,
}

// SAFETY: used from one thread at a time
unsafe impl Send for PreviewScaler {}

impl Default for PreviewScaler {
    fn default() -> Self {
        PreviewScaler { sws: ptr::null_mut() }
    }
}

impl PreviewScaler {
    pub fn rgb(&mut self, frame: &Frame, out_w: u32, out_h: u32) -> Vec<u8> {
        let (w, h) = (frame.width as i32, frame.height as i32);
        let (cw, ch) = ((w + 1) / 2, (h + 1) / 2);
        let mut rgb = vec![0u8; (out_w * out_h * 3) as usize];
        // SAFETY: plane pointers and strides describe frame.i420 exactly; rgb holds out_w * out_h * 3 bytes
        unsafe {
            self.sws = ff::sws_getCachedContext(self.sws, w, h, ff::AVPixelFormat::AV_PIX_FMT_YUV420P, out_w as i32, out_h as i32,
                                                ff::AVPixelFormat::AV_PIX_FMT_RGB24, ff::SwsFlags::SWS_BILINEAR as i32,
                                                ptr::null_mut(), ptr::null_mut(), ptr::null());
            if self.sws.is_null() {
                return rgb;
            }
            let base = frame.i420.as_ptr();
            let src = [base, base.add((w * h) as usize), base.add((w * h + cw * ch) as usize), ptr::null()];
            let src_stride = [w, cw, cw, 0];
            let dst = [rgb.as_mut_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
            let dst_stride = [(out_w * 3) as i32, 0, 0, 0];
            ff::sws_scale(self.sws, src.as_ptr(), src_stride.as_ptr(), 0, h, dst.as_ptr(), dst_stride.as_ptr());
        }
        rgb
    }
}

impl Drop for PreviewScaler {
    fn drop(&mut self) {
        // SAFETY: made by sws_getCachedContext, freed once
        unsafe { ff::sws_freeContext(self.sws) }
    }
}
