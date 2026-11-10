//! A phone's video over TCP as media wants it: /v1/video.h264 in chunks as they arrive, /v1/video's multipart MJPEG one
//! JPEG per packet (the reading half of h264_reader.py and mjpeg_reader.py; decoding is the media crate's).

use crate::https::{Aborter, PhoneAuth, Request, Response};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::media::{Codec, FeedEnd, FeedStopper, OpenStop, Packet, VideoFeed};

const CHUNK: usize = 64 * 1024;
const MJPEG_CHUNK: usize = 4096;
const MAX_PART_HEADER_BYTES: usize = 4096;
pub(crate) const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

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
        let watch = |a: Aborter| stop.watch(a);
        let resp = auth.open(Request::get(url, timeout).watched(&watch)).map_err(|e| format!("Couldn't open the video: {e}"))?;
        if !resp.content_type().contains("video/h264") {
            return Err(format!("The phone answered with {} instead of H.264", resp.content_type()));
        }
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
    /// What has arrived, of which everything before `pos` has been handed on.
    buf: Vec<u8>,
    pos: usize,
    /// Where a read lands before it joins buf; kept between reads so none asks for a fresh vector the size of what's left of the picture.
    scratch: Vec<u8>,
    stop: StopHandle,
}

/// The most one read asks for, however much of the picture is left.
const MAX_READ: usize = 256 * 1024;

impl MjpegFeed {
    pub fn open(url: &str, auth: &PhoneAuth, timeout: Duration, stop: StopHandle) -> Result<MjpegFeed, String> {
        let watch = |a: Aborter| stop.watch(a);
        let resp = auth.open(Request::get(url, timeout).watched(&watch)).map_err(|e| format!("Couldn't open the video: {e}"))?;
        let kind = resp.content_type().to_string();
        let boundary = boundary_of(&kind).ok_or_else(|| format!("The phone answered with {kind} instead of MJPEG"))?;
        Ok(MjpegFeed::from_reader(Box::new(resp), boundary, stop))
    }

    pub fn from_reader(resp: Box<dyn Read + Send>, boundary: Vec<u8>, stop: StopHandle) -> MjpegFeed {
        MjpegFeed { resp, boundary, buf: Vec::new(), pos: 0, scratch: Vec::new(), stop }
    }

    /// What has arrived and not been handed on.
    fn unread(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    /// Whatever has arrived, at most n bytes; false at the end.
    fn recv(&mut self, n: usize) -> std::io::Result<bool> {
        let n = n.min(MAX_READ);
        if self.scratch.len() < n {
            self.scratch.resize(n, 0);
        }
        let got = self.resp.read(&mut self.scratch[..n])?;
        self.buf.extend_from_slice(&self.scratch[..got]);
        Ok(got > 0)
    }

    fn read_line(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(i) = self.unread().windows(2).position(|w| w == b"\r\n") {
                let line = self.unread()[..i].to_vec();
                self.pos += i + 2;
                return Ok(Some(line));
            }
            if self.unread().len() > MAX_PART_HEADER_BYTES || !self.recv(MJPEG_CHUNK)? {
                return Ok(None);
            }
        }
    }

    /// The next part's JPEG, or None when the stream ends or breaks the rules.
    pub fn read_part(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        // What the last part left behind (its CRLF and a little of what follows) moves to the front, once a part rather than once a line
        self.buf.drain(..self.pos);
        self.pos = 0;
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
        while self.unread().len() < length {
            let want = (length - self.unread().len()).max(MJPEG_CHUNK);
            if !self.recv(want)? {
                return Ok(None);
            }
        }
        // The part's trailing CRLF is left for the next boundary scan, which skips it as an empty line
        let part = self.unread()[..length].to_vec();
        self.pos += length;
        Ok(Some(part))
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
    open_feed_stoppable(url, auth, timeout, &OpenStop::default())
}

/// open_feed, which `open.stop()` ends wherever it is, including a connect, handshake or head still pending.
pub fn open_feed_stoppable(url: &str, auth: &PhoneAuth, timeout: Duration, open: &OpenStop) -> Result<(Codec, Box<dyn VideoFeed>), String> {
    let path = crate::https::Url::parse(url).map(|u| u.target).unwrap_or_default();
    let path = path.split('?').next().unwrap_or("");
    let stop = StopHandle::default();
    let handle = stop.clone();
    open.watch(move || handle.stop());
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

    // test_packets.py::test_the_stream_worker_gives_its_tap_to_light_readers_only: _new_reader goes by the URL, as this does
    #[test]
    fn a_feed_goes_by_what_the_url_asks_for() {
        // UDP that doesn't get going falls back to the same feed on .h264, so .udp asks for it here too
        let server = serve(None, Arc::new(|req| if req.path.contains(".h264") || req.path.contains(".udp") {
            (200, "video/h264".into(), b"\x00\x00\x00\x01".to_vec())
        } else {
            (200, "multipart/x-mixed-replace; boundary=--frame".into(), Vec::new())
        }));
        let open = |path: &str| open_feed(&format!("{}{path}", server.base(false)), &PhoneAuth::new("t", ""), Duration::from_secs(3)).map(|(codec, _)| codec);
        assert_eq!(open("/v1/video.h264"), Ok(Codec::H264));
        assert_eq!(open("/v1/video.h264?x=1"), Ok(Codec::H264));
        assert_eq!(open("/v1/video.udp"), Ok(Codec::H264));
        assert_eq!(open("/v1/video.mjpeg"), Ok(Codec::Mjpeg));
        assert_eq!(open("/v1/video"), Ok(Codec::Mjpeg));
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

    /// A phone that takes the connection and then says nothing, so the open sits waiting for the head; tells when it has accepted.
    fn stalled_phone() -> (String, std::sync::mpsc::Receiver<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/v1/video.h264", listener.local_addr().unwrap().port());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (_conn, _) = listener.accept().unwrap();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(10));
        });
        (url, rx)
    }

    #[test]
    fn a_stop_reaches_a_feed_whose_phone_accepted_and_then_stalled() {
        let (url, accepted) = stalled_phone();
        let open = OpenStop::default();
        let stop = open.clone();
        let opening = std::thread::spawn(move || open_feed_stoppable(&url, &PhoneAuth::new("t", ""), Duration::from_secs(30), &open).err());
        // Accepted means the client is connected, so its aborter is registered already
        accepted.recv().unwrap();
        let started = std::time::Instant::now();
        stop.stop();
        assert!(opening.join().unwrap().is_some());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_stop_before_the_open_has_begun_ends_it_at_once() {
        let (url, _accepted) = stalled_phone();
        let open = OpenStop::default();
        open.stop();
        let started = std::time::Instant::now();
        assert!(open_feed_stoppable(&url, &PhoneAuth::new("t", ""), Duration::from_secs(30), &open).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

#[cfg(test)]
mod mjpeg_bench {
    use super::*;
    use std::time::Instant;

    /// A reader that gives at most `cap` bytes a read, as TLS and a socket do.
    struct Capped {
        data: Vec<u8>,
        at: usize,
        cap: usize,
    }

    impl Read for Capped {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.cap).min(self.data.len() - self.at);
            buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    fn stream(frames: usize, size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for f in 0..frames {
            let jpeg: Vec<u8> = (0..size).map(|i| (i.wrapping_mul(31).wrapping_add(f)) as u8).collect();
            out.extend(format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len()).into_bytes());
            out.extend(jpeg);
            out.extend(b"\r\n");
        }
        out
    }

    #[test]
    #[ignore = "timed: cargo test -p telescope-link --release mjpeg_bench -- --ignored --nocapture"]
    fn multipart_parsing_time_by_picture_size_and_read_size() {
        let frames = 100;
        for (name, size) in [("1080p JPEG 400 KB", 400_000), ("4K JPEG 1.6 MB", 1_600_000)] {
            let data = stream(frames, size);
            for cap in [4096, 16 * 1024, 64 * 1024] {
                let best = (0..5).map(|_| {
                    let mut feed = MjpegFeed::from_reader(Box::new(Capped { data: data.clone(), at: 0, cap }), b"--frame".to_vec(), StopHandle::default());
                    let began = Instant::now();
                    let mut n = 0;
                    while feed.read().is_ok() {
                        n += 1;
                    }
                    assert_eq!(n, frames);
                    began.elapsed().as_secs_f64() * 1e3 / frames as f64
                }).fold(f64::MAX, f64::min);
                println!("{name:<20} reads of {:>2} KiB: {best:.3} ms a picture", cap / 1024);
            }
        }
    }
}

#[cfg(test)]
mod mjpeg_same {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Gives at most a seeded random few bytes a read, and counts the reads.
    struct Choppy {
        data: Vec<u8>,
        at: usize,
        seed: u64,
        max: usize,
        reads: Arc<AtomicUsize>,
    }

    impl Read for Choppy {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let cap = 1 + (self.seed >> 33) as usize % self.max;
            let n = buf.len().min(cap).min(self.data.len() - self.at);
            buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    /// The parser as it was before it kept one buffer with a cursor: a vector for every read and a drain from the front for every line and part.
    struct Old {
        resp: Box<dyn Read + Send>,
        boundary: Vec<u8>,
        buf: Vec<u8>,
    }

    impl Old {
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

        fn read_part(&mut self) -> std::io::Result<Option<Vec<u8>>> {
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
            Ok(Some(self.buf.drain(..length).collect()))
        }
    }

    /// A stream with every kind of part and mess in it, from a seed.
    fn stream(seed: u64) -> Vec<u8> {
        let mut s = seed;
        let mut next = move |n: usize| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as usize % n
        };
        let mut out = Vec::new();
        for _ in 0..(3 + next(6)) {
            match next(10) {
                // Junk before the boundary, and a part with no length
                0 => out.extend(b"preamble\r\nmore\r\n"),
                1 => out.extend(b"--frame\r\nContent-Type: image/jpeg\r\n\r\n"),
                // A part whose headers are huge
                2 => out.extend(format!("--frame\r\nX-Pad: {}\r\n\r\n", "x".repeat(5000)).into_bytes()),
                _ => {
                    let size = 1 + next(30_000);
                    let jpeg: Vec<u8> = (0..size).map(|i| (i * 7 + size) as u8).collect();
                    out.extend(format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {size}\r\n\r\n").into_bytes());
                    out.extend(jpeg);
                    out.extend(b"\r\n");
                }
            }
        }
        // Sometimes the end is cut short
        if next(3) == 0 {
            let cut = out.len() - next(out.len().min(2000));
            out.truncate(cut);
        }
        out
    }

    fn parts<F: FnMut() -> std::io::Result<Option<Vec<u8>>>>(mut read: F) -> Vec<Option<Vec<u8>>> {
        let mut got = Vec::new();
        while let Ok(part) = read() {
            let end = part.is_none();
            got.push(part);
            if end {
                break;
            }
        }
        got
    }

    #[test]
    fn the_parts_and_the_reads_made_are_the_same_as_before_for_any_stream_and_read_size() {
        let mut seen = 0;
        for seed in 0..300u64 {
            let data = stream(seed);
            for max in [1usize, 7, 4096, 5000, 65_536] {
                let (old_reads, new_reads) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
                let choppy = |reads: &Arc<AtomicUsize>| Choppy { data: data.clone(), at: 0, seed, max, reads: reads.clone() };
                let mut old = Old { resp: Box::new(choppy(&old_reads)), boundary: b"--frame".to_vec(), buf: Vec::new() };
                let mut new = MjpegFeed::from_reader(Box::new(choppy(&new_reads)), b"--frame".to_vec(), StopHandle::default());
                let (a, b) = (parts(|| old.read_part()), parts(|| new.read_part()));
                assert_eq!(a, b, "seed {seed}, reads of up to {max}");
                assert_eq!(old_reads.load(Ordering::SeqCst), new_reads.load(Ordering::SeqCst), "seed {seed}, reads of up to {max}");
                seen += a.iter().flatten().count();
            }
        }
        assert!(seen > 1000, "{seen}");
    }
}
