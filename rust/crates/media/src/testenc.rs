//! Test streams made with this FFmpeg's encoders: H.264 framed like the phone sends it, and JPEGs.

use ffmpeg_sys_next as ff;
use std::ffi::CString;
use std::ptr;

const AUD: &[u8] = b"\x00\x00\x00\x01\x09\xf0";

/// Encodes flat frames (Y as each value is, every U and V the same) with the given pixel format and options.
fn encode(name: &str, values: &[u8], chroma: (u8, u8), w: u32, h: u32, fmt: ff::AVPixelFormat, opts: &[(&str, &str)]) -> Option<Vec<Vec<u8>>> {
    // SAFETY: plain libavcodec encoding; everything allocated here is freed before returning
    unsafe {
        let cname = CString::new(name).unwrap();
        let codec = ff::avcodec_find_encoder_by_name(cname.as_ptr());
        if codec.is_null() {
            eprintln!("this FFmpeg has no {name} encoder; skipped");
            return None;
        }
        let mut ctx = ff::avcodec_alloc_context3(codec);
        (*ctx).width = w as i32;
        (*ctx).height = h as i32;
        (*ctx).pix_fmt = fmt;
        (*ctx).time_base = ff::AVRational { num: 1, den: 30 };
        let mut dict = ptr::null_mut();
        for (k, v) in opts {
            let (k, v) = (CString::new(*k).unwrap(), CString::new(*v).unwrap());
            ff::av_dict_set(&mut dict, k.as_ptr(), v.as_ptr(), 0);
        }
        let r = ff::avcodec_open2(ctx, codec, &mut dict);
        ff::av_dict_free(&mut dict);
        if r < 0 {
            ff::avcodec_free_context(&mut ctx);
            eprintln!("the {name} encoder didn't open; skipped");
            return None;
        }
        let mut frame = ff::av_frame_alloc();
        (*frame).width = w as i32;
        (*frame).height = h as i32;
        (*frame).format = fmt as i32;
        ff::av_frame_get_buffer(frame, 0);
        let mut pkt = ff::av_packet_alloc();
        let mut out = Vec::new();
        let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
        for (i, &v) in values.iter().enumerate() {
            ff::av_frame_make_writable(frame);
            let y = v;
            for (plane, value, pw, ph) in [(0, y, w as usize, h as usize), (1, chroma.0, cw, ch), (2, chroma.1, cw, ch)] {
                let (data, stride) = ((*frame).data[plane], (*frame).linesize[plane] as usize);
                for row in 0..ph {
                    ptr::write_bytes(data.add(row * stride), value, pw);
                }
            }
            (*frame).pts = i as i64;
            ff::avcodec_send_frame(ctx, frame);
            while ff::avcodec_receive_packet(ctx, pkt) == 0 {
                out.push(std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize).to_vec());
                ff::av_packet_unref(pkt);
            }
        }
        ff::avcodec_send_frame(ctx, ptr::null());
        while ff::avcodec_receive_packet(ctx, pkt) == 0 {
            out.push(std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize).to_vec());
            ff::av_packet_unref(pkt);
        }
        ff::av_packet_free(&mut pkt);
        ff::av_frame_free(&mut frame);
        ff::avcodec_free_context(&mut ctx);
        Some(out)
    }
}

/// Annex-B H.264 of flat grey frames, one packet per value, framed like the phone sends them: no B-frames, and a
/// delimiter after each packet. gop: frames between keyframes.
pub fn h264(values: &[u8], w: u32, h: u32, gop: u32) -> Option<Vec<Vec<u8>>> {
    let g = gop.to_string();
    // As BGR greys turn into YUV: the limited range
    let limited: Vec<u8> = values.iter().map(|&v| (16.0 + v as f64 * 219.0 / 255.0).round() as u8).collect();
    let packets = encode("libx264", &limited, (128, 128), w, h, ff::AVPixelFormat::AV_PIX_FMT_YUV420P,
                         &[("tune", "zerolatency"), ("g", &g), ("bf", "0"), ("preset", "ultrafast")])?;
    Some(packets.into_iter().map(|p| [p, AUD.to_vec()].concat()).collect())
}

/// A JPEG of a flat grey.
pub fn jpeg(value: u8, w: u32, h: u32) -> Vec<u8> {
    encode("mjpeg", &[value], (128, 128), w, h, ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P, &[]).and_then(|mut p| p.pop()).expect("FFmpeg always has its MJPEG encoder")
}

/// One flat H.264 picture with these Y, U and V values as they are in the stream, saying in its headers what matrix
/// ("bt709") and range ("tv" or "pc") they are in. Framed as h264() is.
pub fn h264_yuv(y: u8, u: u8, v: u8, w: u32, h: u32, matrix: &str, range: &str) -> Option<Vec<Vec<u8>>> {
    let packets = encode("libx264", &[y], (u, v), w, h, ff::AVPixelFormat::AV_PIX_FMT_YUV420P,
                         &[("tune", "zerolatency"), ("bf", "0"), ("preset", "ultrafast"), ("colorspace", matrix), ("color_range", range)])?;
    Some(packets.into_iter().map(|p| [p, AUD.to_vec()].concat()).collect())
}

/// A JPEG of a flat colour with these full-range Y, U and V values.
pub fn jpeg_yuv(y: u8, u: u8, v: u8, w: u32, h: u32) -> Vec<u8> {
    encode("mjpeg", &[y], (u, v), w, h, ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P, &[]).and_then(|mut p| p.pop()).expect("FFmpeg always has its MJPEG encoder")
}
