//! A phone's video over TCP as media wants it: /v1/video.h264 in chunks as they arrive, /v1/video's multipart MJPEG one
//! JPEG per packet (the reading half of h264_reader.py and mjpeg_reader.py; decoding is the media crate's).

use crate::https::{Aborter, PhoneAuth, Request, Response};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::media::{Codec, FeedEnd, FeedStopper, Packet, VideoFeed};

const CHUNK: usize = 64 * 1024;
const MJPEG_CHUNK: usize = 4096;
const MAX_PART_HEADER_BYTES: usize = 4096;
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

fn packet(data: Vec<u8>) -> Packet {
    Packet { data, time_ns: telescope_core::time::monotonic_ns() }
}

/// Ends a feed's read from another thread: whichever response it reads now, and any it opens after.
#[derive(Clone, Default)]
pub struct StopHandle {
    state: Arc<Mutex<(bool, Option<Aborter>)>>,
}

impl StopHandle {
    pub fn stopped(&self) -> bool {
        self.state.lock().unwrap().0
    }

    /// The response the feed reads from now; aborted at once if it's stopped already.
    pub fn watch(&self, aborter: Aborter) {
        let mut st = self.state.lock().unwrap();
        if st.0 {
            aborter.abort();
        }
        st.1 = Some(aborter);
    }
}

impl FeedStopper for StopHandle {
    fn stop(&self) {
        let mut st = self.state.lock().unwrap();
        st.0 = true;
        if let Some(a) = st.1.take() {
            a.abort();
        }
    }
}

fn end_of(e: std::io::Error, stop: &StopHandle) -> FeedEnd {
    if stop.stopped() { FeedEnd::Stopped } else { FeedEnd::Error(e.to_string()) }
}

/// /v1/video.h264 as it arrives: Annex-B in chunks that needn't line up with access units.
pub struct H264Feed {
    resp: Response,
    buf: Vec<u8>,
    stop: StopHandle,
}

impl H264Feed {
    /// None when it doesn't open or the phone answers with something other than H.264.
    pub fn open(url: &str, auth: &PhoneAuth, timeout: Duration, stop: StopHandle) -> Result<H264Feed, String> {
        let resp = auth.open(Request::get(url, timeout)).map_err(|e| format!("Couldn't open the video: {e}"))?;
        if !resp.content_type().contains("video/h264") {
            return Err(format!("The phone answered with {} instead of H.264", resp.content_type()));
        }
        stop.watch(resp.aborter());
        Ok(H264Feed { resp, buf: vec![0; CHUNK], stop })
    }
}

impl VideoFeed for H264Feed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        match self.resp.read(&mut self.buf) {
            Ok(0) if self.stop.stopped() => Err(FeedEnd::Stopped),
            Ok(0) => Err(FeedEnd::Closed),
            Ok(n) => Ok(packet(self.buf[..n].to_vec())),
            Err(e) => Err(end_of(e, &self.stop)),
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(self.stop.clone())
    }
}

/// /v1/video's multipart/x-mixed-replace, one JPEG per packet.
pub struct MjpegFeed {
    resp: Box<dyn Read + Send>,
    boundary: Vec<u8>,
    buf: Vec<u8>,
    stop: StopHandle,
}

impl MjpegFeed {
    pub fn open(url: &str, auth: &PhoneAuth, timeout: Duration, stop: StopHandle) -> Result<MjpegFeed, String> {
        let resp = auth.open(Request::get(url, timeout)).map_err(|e| format!("Couldn't open the video: {e}"))?;
        let kind = resp.content_type().to_string();
        let boundary = boundary_of(&kind).ok_or_else(|| format!("The phone answered with {kind} instead of MJPEG"))?;
        stop.watch(resp.aborter());
        Ok(MjpegFeed::from_reader(Box::new(resp), boundary, stop))
    }

    pub fn from_reader(resp: Box<dyn Read + Send>, boundary: Vec<u8>, stop: StopHandle) -> MjpegFeed {
        MjpegFeed { resp, boundary, buf: Vec::new(), stop }
    }

    /// Whatever has arrived, at most n bytes; false at the end.
    fn recv(&mut self, n: usize) -> std::io::Result<bool> {
        let mut chunk = vec![0u8; n];
        let got = self.resp.read(&mut chunk)?;
        self.buf.extend_from_slice(&chunk[..got]);
        Ok(got > 0)
    }

    fn read_line(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(i) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let line = self.buf[..i].to_vec();
                self.buf.drain(..i + 2);
                return Ok(Some(line));
            }
            if self.buf.len() > MAX_PART_HEADER_BYTES || !self.recv(MJPEG_CHUNK)? {
                return Ok(None);
            }
        }
    }

    /// The next part's JPEG, or None when the stream ends or breaks the rules.
    pub fn read_part(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            let Some(line) = self.read_line()? else { return Ok(None) };
            let stripped = line.trim_ascii();
            if stripped == self.boundary.as_slice() || stripped == [self.boundary.as_slice(), b"--"].concat().as_slice() {
                break;
            }
        }
        let mut length: Option<i64> = None;
        loop {
            let Some(line) = self.read_line()? else { return Ok(None) };
            if line.is_empty() {
                break;
            }
            if line.to_ascii_lowercase().starts_with(b"content-length:") {
                length = std::str::from_utf8(&line[15..]).ok().and_then(|v| v.trim().parse().ok());
            }
        }
        let Some(length) = length.filter(|n| *n > 0 && *n as usize <= MAX_FRAME_BYTES).map(|n| n as usize) else { return Ok(None) };
        while self.buf.len() < length {
            let want = (length - self.buf.len()).max(MJPEG_CHUNK);
            if !self.recv(want)? {
                return Ok(None);
            }
        }
        // The part's trailing CRLF is left for the next boundary scan, which skips it as an empty line
        Ok(Some(self.buf.drain(..length).collect()))
    }
}

/// The boundary from a multipart Content-Type, used as it is (the phone's includes the leading "--").
pub fn boundary_of(content_type: &str) -> Option<Vec<u8>> {
    if !content_type.contains("multipart/x-mixed-replace") {
        return None;
    }
    let (_, b) = content_type.split_once("boundary=")?;
    Some(b.trim().trim_matches('"').as_bytes().to_vec())
}

impl VideoFeed for MjpegFeed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        match self.read_part() {
            Ok(Some(jpeg)) => Ok(packet(jpeg)),
            Ok(None) if self.stop.stopped() => Err(FeedEnd::Stopped),
            Ok(None) => Err(FeedEnd::Closed),
            Err(e) => Err(end_of(e, &self.stop)),
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(self.stop.clone())
    }
}

/// The feed for a video URL, by its path: UDP (falling back to TCP), H.264 over TCP, or MJPEG.
pub fn open_feed(url: &str, auth: &PhoneAuth, timeout: Duration) -> Result<(Codec, Box<dyn VideoFeed>), String> {
    let path = crate::https::Url::parse(url).map(|u| u.target).unwrap_or_default();
    let path = path.split('?').next().unwrap_or("");
    let stop = StopHandle::default();
    if path.ends_with(".udp") {
        Ok((Codec::H264, Box::new(crate::udp::UdpFeed::open(url, auth, timeout, stop)?)))
    } else if path.ends_with(".h264") {
        Ok((Codec::H264, Box::new(H264Feed::open(url, auth, timeout, stop)?)))
    } else {
        Ok((Codec::Mjpeg, Box::new(MjpegFeed::open(url, auth, timeout, stop)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::testing::*;

    fn mjpeg(stream: &[u8]) -> MjpegFeed {
        MjpegFeed::from_reader(Box::new(std::io::Cursor::new(stream.to_vec())), b"--frame".to_vec(), StopHandle::default())
    }

    fn part(jpeg: &[u8]) -> Vec<u8> {
        let mut p = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len()).into_bytes();
        p.extend(jpeg);
        p.extend(b"\r\n");
        p
    }

    #[test]
    fn reads_authenticated_frames_in_order_then_reports_eof() {
        let body: Vec<u8> = [part(b"one"), part(b"two"), part(b"three")].concat();
        let body: &'static [u8] = Box::leak(body.into_boxed_slice());
        let server = serve(None, Arc::new(move |req| if req.auth.as_deref() == Some("Bearer tok") {
            (200, "multipart/x-mixed-replace; boundary=--frame".into(), body.to_vec())
        } else {
            (401, "text/plain".into(), Vec::new())
        }));
        let url = format!("{}/v1/video", server.base(false));
        let (codec, mut feed) = open_feed(&url, &PhoneAuth::new("tok", ""), Duration::from_secs(3)).unwrap();
        assert_eq!(codec, Codec::Mjpeg);
        for want in [&b"one"[..], b"two", b"three"] {
            assert_eq!(feed.read().unwrap().data, want);
        }
        assert_eq!(feed.read().err(), Some(FeedEnd::Closed));
        assert!(open_feed(&url, &PhoneAuth::new("wrong", ""), Duration::from_secs(3)).is_err());
    }

    #[test]
    fn rejects_non_multipart_response() {
        let server = serve(None, Arc::new(|_| (200, "text/html".into(), b"<html>".to_vec())));
        assert!(open_feed(&format!("{}/v1/video", server.base(false)), &PhoneAuth::new("t", ""), Duration::from_secs(3)).is_err());
        assert!(open_feed(&format!("{}/v1/video.h264", server.base(false)), &PhoneAuth::new("t", ""), Duration::from_secs(3)).is_err());
    }

    #[test]
    fn open_returns_false_on_connection_failure() {
        assert!(open_feed("http://127.0.0.1:1/v1/video", &PhoneAuth::new("t", ""), Duration::from_secs(1)).is_err());
    }

    #[test]
    fn returns_a_frame_without_waiting_for_the_next_one() {
        // A reader that hands one part and then blocks would hang if the parser asked for more than had arrived
        struct OnePart(Vec<u8>, bool);
        impl Read for OnePart {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                assert!(!self.1, "read past the frame that had arrived");
                let n = buf.len().min(self.0.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0.drain(..n);
                self.1 = self.0.is_empty();
                Ok(n)
            }
        }
        let mut feed = MjpegFeed::from_reader(Box::new(OnePart(part(b"jpeg"), false)), b"--frame".to_vec(), StopHandle::default());
        assert_eq!(feed.read_part().unwrap().unwrap(), b"jpeg");
    }

    #[test]
    fn a_part_claiming_a_huge_length_ends_the_stream_instead_of_buffering() {
        let stream = format!("--frame\r\nContent-Length: {}\r\n\r\nxx", MAX_FRAME_BYTES + 1);
        assert_eq!(mjpeg(stream.as_bytes()).read_part().unwrap(), None);
    }

    #[test]
    fn a_negative_or_zero_length_part_ends_the_stream() {
        for n in ["-5", "0"] {
            assert_eq!(mjpeg(format!("--frame\r\nContent-Length: {n}\r\n\r\n").as_bytes()).read_part().unwrap(), None);
        }
    }

    #[test]
    fn the_boundary_is_taken_as_the_phone_writes_it() {
        assert_eq!(boundary_of("multipart/x-mixed-replace; boundary=\"--frame\""), Some(b"--frame".to_vec()));
        assert_eq!(boundary_of("multipart/x-mixed-replace"), None);
        assert_eq!(boundary_of("image/jpeg; boundary=x"), None);
    }

    #[test]
    fn an_h264_feed_reads_authenticated_bytes_then_reports_eof() {
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let sent = body.clone();
        let server = serve(None, Arc::new(move |req| if req.auth.as_deref() == Some("Bearer secret") {
            (200, "video/h264".into(), sent.clone())
        } else {
            (401, "text/plain".into(), Vec::new())
        }));
        let url = format!("{}/v1/video.h264", server.base(false));
        assert!(open_feed(&url, &PhoneAuth::new("wrong", ""), Duration::from_secs(3)).is_err());
        let (_, mut feed) = open_feed(&url, &PhoneAuth::new("secret", ""), Duration::from_secs(3)).unwrap();
        let mut got = Vec::new();
        let end = loop {
            match feed.read() {
                Ok(p) => got.extend(p.data),
                Err(e) => break e,
            }
        };
        assert_eq!((got, end), (body, FeedEnd::Closed));
    }

    #[test]
    fn a_reader_decodes_authenticated_frames_then_reports_eof() {
        use telescope_core::media::{DecodeOptions, Media};
        let Some(packets) = telescope_media::test_h264(&[30, 90, 150, 210], 64, 48, 5, false) else { return };
        let body = packets.concat();
        let (sent, length) = (body.clone(), body.len() as u64);
        let server = serve(None, Arc::new(move |req| if req.auth.as_deref() == Some("Bearer secret") {
            (200, "video/h264".into(), sent.clone())
        } else {
            (401, "text/plain".into(), Vec::new())
        }));
        let url = format!("{}/v1/video.h264", server.base(false));
        assert!(open_feed(&url, &PhoneAuth::new("wrong", ""), Duration::from_secs(3)).is_err());
        let (codec, feed) = open_feed(&url, &PhoneAuth::new("secret", ""), Duration::from_secs(3)).unwrap();
        let media = telescope_media::FfmpegMedia;
        let decoder = media.decoder(codec, &DecodeOptions::default()).unwrap();
        let mut reader = media.frame_reader(feed, decoder).unwrap();
        let (mut frames, mut total) = (Vec::new(), 0);
        let end = loop {
            match reader.read() {
                Ok(read) => {
                    total += read.bytes;
                    frames.push(read.frame);
                }
                Err(end) => break end,
            }
        };
        assert_eq!(end, FeedEnd::Closed);
        assert!(!frames.is_empty() && (frames[0].width, frames[0].height) == (64, 48));
        assert!(total > 0 && total <= length, "{total} of {length}");
    }

    #[test]
    fn an_h264_feed_hands_on_what_arrives_and_a_stop_ends_it() {
        let server = serve(None, Arc::new(|_| (200, "video/h264".into(), vec![0, 0, 0, 1, 9, 0xf0])));
        let (codec, mut feed) = open_feed(&format!("{}/v1/video.h264", server.base(false)), &PhoneAuth::new("t", ""), Duration::from_secs(3)).unwrap();
        assert_eq!(codec, Codec::H264);
        assert_eq!(feed.read().unwrap().data, [0, 0, 0, 1, 9, 0xf0]);
        feed.stopper().stop();
        assert_eq!(feed.read().err(), Some(FeedEnd::Stopped));
    }
}
