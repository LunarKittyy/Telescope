// test_stream.py on the real parts of a stream: the worker with media's own pipeline and frame store, and link's feed over a
// real connection. Only the camera at the end is a stand-in.
use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::media::{Codec, Decoded, DecodeOptions, Decoder, FeedEnd, FeedStopper, FrameSlot, Media, Packet, StagedStep, VideoFeed};
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_link::feeds::open_feed;
use telescope_link::https::PhoneAuth;
use telescope_media::FfmpegMedia;
use telescope_shell::worker::{Events, Opened, Opener, StreamWorker, WorkerEvent, WorkerSetup};

struct Cam;

impl VideoOutput for Cam {
    fn send(&mut self, _frame: &Frame) -> Result<(), String> {
        Ok(())
    }
    fn size(&self) -> (u32, u32) {
        (4, 2)
    }
    fn device(&self) -> String {
        "/dev/video9".into()
    }
}

struct Cams;

impl Outputs for Cams {
    fn slots(&self) -> Vec<Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Ok(Box::new(Cam))
    }
}

type Said = Arc<Mutex<Vec<WorkerEvent>>>;

fn start(opener: Opener, media: Arc<dyn Media>, steps: Vec<StagedStep>) -> (StreamWorker, Arc<dyn FrameSlot>, Said) {
    let setup = WorkerSetup { url: "http://phone/v1/video".into(), opener, width: None, height: None, fps: 30, canvas: (None, None), slot: 0, source_id: "p".into(),
                              hardware_decode: false, plugin_source: false, waiting_text: None, vision: None, steps, gate: None };
    let slot = FfmpegMedia.stream_frames().expect("media's frame store").new_slot();
    let said: Said = Arc::default();
    let log = said.clone();
    let events: Events = Arc::new(move |e| log.lock().unwrap().push(e));
    (StreamWorker::start(setup, media, Arc::new(Cams), slot.clone(), events).unwrap(), slot, said)
}

fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !cond() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(cond(), "{what}");
}

/// A feed of packets whose first byte is the picture's grey, one every 5 ms until it's stopped.
struct Packets(u8, Arc<AtomicBool>);

struct Stop(Arc<AtomicBool>);

impl FeedStopper for Stop {
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl VideoFeed for Packets {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        std::thread::sleep(Duration::from_millis(5));
        if self.1.load(Ordering::SeqCst) {
            return Err(FeedEnd::Stopped);
        }
        Ok(Packet { data: vec![self.0], time_ns: 0 })
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(Stop(self.1.clone()))
    }
}

struct Grey;

impl Decoder for Grey {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        Decoded { newest: Some(Frame::new(4, 2, PixelFormat::I420, vec![packet.data[0]; 12])), count: 1 }
    }
    fn hardware(&self) -> bool {
        false
    }
}

/// Media's real pipeline behind a decoder that needs no real video.
struct Plain;

impl Media for Plain {
    fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        Ok(Box::new(Grey))
    }
    fn scaler(&self) -> Box<dyn telescope_core::media::Scaler> {
        FfmpegMedia.scaler()
    }
    fn pipeline(&self) -> Result<Box<dyn telescope_core::media::FramePipeline>, String> {
        FfmpegMedia.pipeline()
    }
}

fn flowing(grey: u8) -> Opener {
    Arc::new(move |_| Ok((Codec::Mjpeg, Box::new(Packets(grey, Arc::default())) as Box<dyn VideoFeed>).into()))
}

struct Maths(Box<dyn Fn(u8) -> u8 + Send>);

impl CpuStep for Maths {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let value = (self.0)(frame.data[0]);
        frame.data_mut()[0] = value;
    }
}

fn step(maths: impl Fn(u8) -> u8 + Send + 'static) -> StagedStep {
    StagedStep { owner: "maths".into(), stage: FrameStage::Effects, step: FrameStep::Cpu(Box::new(Maths(Box::new(maths)))) }
}

#[test]
fn a_new_pipeline_takes_over_from_the_next_frame() {
    let (w, slot, _said) = start(flowing(2), Arc::new(Plain), vec![step(|v| v + 1)]);
    let picture = || slot.latest().map(|(f, _)| f.data[0]);
    wait_for("the first steps", || picture() == Some(3));
    w.set_steps(vec![step(|v| v * 10)]);
    wait_for("the new steps", || picture() == Some(20));
    let (_, seen) = slot.latest().unwrap();
    wait_for("the frames after", || slot.latest().unwrap().1 > seen + 2);
    assert_eq!(picture(), Some(20), "the old steps are gone, not run as well");
    w.stop();
}

#[test]
fn worker_processes_pipeline_in_order() {
    let (w, slot, _said) = start(flowing(1), Arc::new(Plain), vec![step(|v| v + 2), step(|v| v * 3)]);
    wait_for("a frame through both", || slot.latest().is_some());
    assert_eq!(slot.latest().unwrap().0.data[0], 9, "(1 + 2) * 3");
    w.stop();
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

/// A phone that answers the token with JPEGs until the connection goes, and says which paths it was asked for.
fn phone(token: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 95).encode(&[200; 16 * 8 * 3], 16, 8, ExtendedColorType::Rgb8).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let heard = seen.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let auth = read_request(&stream);
            heard.lock().unwrap().push(auth.clone());
            if auth != format!("Bearer {token}") {
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
                continue;
            }
            let jpeg = jpeg.clone();
            std::thread::spawn(move || {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=--mjpegframe\r\n\r\n");
                loop {
                    let head = format!("--mjpegframe\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len());
                    let sent = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&jpeg)).and_then(|_| stream.write_all(b"\r\n"));
                    if sent.is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        }
    });
    (format!("http://127.0.0.1:{port}/v1/video"), seen)
}

// Session::open_video is this one line on top of a pinned HTTPS connection, which link's https tests cover
fn opener(url: String, token: &'static str) -> Opener {
    Arc::new(move |_| open_feed(&url, &PhoneAuth::new(token, ""), Duration::from_secs(3)).map(Opened::from))
}

#[test]
fn open_cap_constructs_authenticated_reader_and_opens() {
    let (url, asked) = phone("tok123");
    let (w, slot, said) = start(opener(url.clone(), "tok123"), Arc::new(FfmpegMedia), Vec::new());
    wait_for("a decoded picture", || slot.latest().is_some());
    let (frame, _) = slot.latest().unwrap();
    assert_eq!((frame.width, frame.height), (16, 8));
    assert_eq!(asked.lock().unwrap()[0], "Bearer tok123");
    assert!(!said.lock().unwrap().iter().any(|e| matches!(e, WorkerEvent::Status(k, _) if k == "waiting")));
    w.stop();

    let (w, slot, said) = start(opener(url, "wrong"), Arc::new(FfmpegMedia), Vec::new());
    wait_for("the worker waits", || said.lock().unwrap().iter().any(|e| matches!(e, WorkerEvent::Status(k, _) if k == "waiting")));
    assert!(slot.latest().is_none(), "the phone refused it");
    w.stop();
}
