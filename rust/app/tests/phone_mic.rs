//! The Microphone's worker reading the phone's /v1/audio through link's real request (test_audio.py's worker tests). The phone is a
//! few lines of plain HTTP here; the worker hears what the wire says, not a stand-in for it.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_audio::worker::{AudioWorker, MicSink, Opener, OpenSink};
use telescope_core::audio::AudioFeed;
use telescope_link::audio::open_audio;
use telescope_link::https::PhoneAuth;

const TOKEN: &str = "tok";

/// A phone answering every connection with `answer` (bytes to send); what each request's Authorization header said, in order.
fn phone(answer: Vec<u8>) -> (String, Arc<Mutex<VecDeque<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/v1/audio", listener.local_addr().unwrap().port());
    let seen: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let s = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            serve(conn, &answer, &s);
        }
    });
    (url, seen)
}

fn serve(mut conn: TcpStream, answer: &[u8], seen: &Mutex<VecDeque<String>>) {
    let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
    while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).is_ok_and(|n| n == 1) {
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let auth = head.lines().find_map(|l| l.strip_prefix("Authorization: ")).unwrap_or_default().to_string();
    seen.lock().unwrap().push_back(auth);
    let _ = conn.write_all(answer);
}

fn opener(url: &str) -> Opener {
    let url = url.to_string();
    Arc::new(move || open_audio(&url, &PhoneAuth::new(TOKEN, "")).map(|feed| Box::new(feed) as Box<dyn AudioFeed>))
}

#[derive(Clone, Default)]
struct Sink {
    data: Arc<Mutex<Vec<i16>>>,
    closed: Arc<AtomicBool>,
}

struct SinkHandle(Sink);

impl MicSink for SinkHandle {
    fn write(&mut self, samples: &[i16]) -> std::io::Result<()> {
        self.0.data.lock().unwrap().extend_from_slice(samples);
        std::thread::sleep(Duration::from_millis(2));
        Ok(())
    }
}

impl Drop for SinkHandle {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::SeqCst);
    }
}

fn sink_opener(sink: &Sink) -> OpenSink {
    let sink = sink.clone();
    Arc::new(move || Ok(Box::new(SinkHandle(sink.clone())) as Box<dyn MicSink>))
}

type Statuses = Arc<Mutex<Vec<(String, String)>>>;

fn worker(url: &str, sink: &Sink) -> (AudioWorker, Statuses) {
    let statuses = Statuses::default();
    let st = statuses.clone();
    let w = AudioWorker::with(url, opener(url), sink_opener(sink), Arc::new(move |k, t| st.lock().unwrap().push((k.into(), t.into()))),
                              Duration::from_millis(50), Box::new(|| Ok(None)));
    (w, statuses)
}

fn until(cond: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    while !cond() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

#[test]
fn worker_plays_the_phone_audio_into_the_sink_with_the_token() {
    let mut answer = b"HTTP/1.1 200 OK\r\nContent-Type: audio/L16\r\nConnection: close\r\n\r\n".to_vec();
    answer.extend([0x10u8, 0].repeat(200 * 48));
    let (url, seen) = phone(answer);
    let sink = Sink::default();
    let (mut w, statuses) = worker(&url, &sink);
    w.start();
    assert!(until(|| sink.data.lock().unwrap().windows(8).any(|run| run.iter().all(|s| *s == 16))));
    w.stop();
    assert_eq!(seen.lock().unwrap().front().map(String::as_str), Some("Bearer tok"));
    assert!(statuses.lock().unwrap().contains(&("ok".into(), "".into())));
    assert!(sink.closed.load(Ordering::SeqCst));
}

#[test]
fn worker_reports_the_phones_reason_and_keeps_trying() {
    let body = br#"{"error": "Allow the microphone in Telescope on the phone"}"#;
    let mut answer = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    answer.extend(body);
    let (url, seen) = phone(answer);
    let (mut w, statuses) = worker(&url, &Sink::default());
    w.start();
    assert!(until(|| seen.lock().unwrap().len() >= 2), "it keeps trying");
    w.stop();
    assert_eq!(statuses.lock().unwrap()[0], ("err".to_string(), "Allow the microphone in Telescope on the phone".to_string()));
}
