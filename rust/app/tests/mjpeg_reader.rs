// test_mjpeg_reader.py end to end: a real connection, link's feed and media's reader, down to the picture.
use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use telescope_core::image::frame_rgb;
use telescope_core::media::{DecodeOptions, FeedEnd, FrameReader, Media};
use telescope_link::feeds::open_feed;
use telescope_link::https::PhoneAuth;
use telescope_media::FfmpegMedia;

fn grey_jpeg(value: u8) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 95).encode(&[value; 2 * 2 * 3], 2, 2, ExtendedColorType::Rgb8).unwrap();
    out
}

/// The request's Authorization header, once the whole request has come.
fn read_request(stream: &TcpStream) -> String {
    let mut auth = String::new();
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Authorization: ") {
            auth = value.to_string();
        }
    }
    auth
}

fn send_part(stream: &mut TcpStream, jpeg: &[u8]) {
    stream.write_all(format!("--mjpegframe\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len()).as_bytes()).unwrap();
    stream.write_all(jpeg).unwrap();
    stream.write_all(b"\r\n").unwrap();
    stream.flush().unwrap();
}

const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=--mjpegframe\r\n\r\n";

/// A server that answers the token with frames, then waits for hold, if any, and hangs up.
fn serve(token: &'static str, frames: Vec<Vec<u8>>, hold: Option<Receiver<()>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut hold = hold;
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            if read_request(&stream) != format!("Bearer {token}") {
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
                continue;
            }
            stream.write_all(HEAD).unwrap();
            for jpeg in &frames {
                send_part(&mut stream, jpeg);
            }
            if let Some(hold) = hold.take() {
                let _ = hold.recv();
            }
        }
    });
    format!("http://127.0.0.1:{port}/v1/video")
}

fn opened(url: &str, token: &str) -> Result<Box<dyn FrameReader>, String> {
    let (codec, feed) = open_feed(url, &PhoneAuth::new(token, ""), Duration::from_secs(3))?;
    let decoder = FfmpegMedia.decoder(codec, &DecodeOptions::default())?;
    FfmpegMedia.frame_reader(feed, decoder)
}

fn close_to(pixels: &[u8], value: u8) -> bool {
    pixels.iter().all(|p| p.abs_diff(value) <= 2)
}

// test_mjpeg_reader.py::test_reads_authenticated_frames_in_order_then_reports_eof
#[test]
fn reads_authenticated_frames_in_order_then_reports_eof() {
    let url = serve("secret", vec![grey_jpeg(20), grey_jpeg(200)], None);
    assert!(opened(&url, "wrong").is_err());
    let mut reader = opened(&url, "secret").unwrap();
    let first = reader.read().unwrap().frame;
    assert_eq!((first.width, first.height), (2, 2));
    let rgb = frame_rgb(&first).unwrap();
    assert!(close_to(&rgb, 20), "{rgb:?}");
    let rgb = frame_rgb(&reader.read().unwrap().frame).unwrap();
    assert!(close_to(&rgb, 200), "{rgb:?}");
    assert_eq!(reader.read().err(), Some(FeedEnd::Closed));
}

// test_mjpeg_reader.py::test_returns_a_frame_without_waiting_for_the_next_one
#[test]
fn returns_a_frame_without_waiting_for_the_next_one() {
    let (release, hold) = channel();
    let url = serve("t", vec![grey_jpeg(20)], Some(hold));
    let mut reader = opened(&url, "t").unwrap();
    let started = Instant::now();
    let read = reader.read().unwrap().frame;
    let elapsed = started.elapsed();
    assert_eq!((read.width, read.height), (2, 2));
    assert!(elapsed < Duration::from_secs(1), "a read waited {elapsed:?} for the next frame");
    // release(): a read stuck on the open connection ends
    let stopper = reader.stopper();
    let reading = std::thread::spawn(move || reader.read().err());
    stopper.stop();
    assert_eq!(reading.join().unwrap(), Some(FeedEnd::Stopped));
    drop(release);
}
