//! test_browser_server.py's worker tests: this build's H.264 and a tone into the feed, through the real stream and audio workers.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_audio::worker::{AudioWorker, MicSink, OpenSink};
use telescope_core::audio::AudioFeed;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::media::{Codec, DecodeOptions, FrameSlot, Media, Packet, StagedStep, VideoFeed};
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_link::browser::feed::{BrowserFeed, BrowserVideo};
use telescope_media::FfmpegMedia;
use telescope_shell::worker::{Events, Opener, StreamWorker, WorkerEvent, WorkerSetup};

const SIZE: (u32, u32) = (96, 64);
const AUD: &[u8] = b"\x00\x00\x00\x01\x09\xf0";

fn wait(what: impl Fn() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while !what() {
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

struct Cam(Arc<Mutex<Vec<Frame>>>, (u32, u32));

impl VideoOutput for Cam {
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        self.0.lock().unwrap().push(frame.clone());
        Ok(())
    }
    fn size(&self) -> (u32, u32) {
        self.1
    }
    fn device(&self) -> String {
        "/dev/video9".into()
    }
}

struct Cams(Arc<Mutex<Vec<Frame>>>);

impl Outputs for Cams {
    fn slots(&self) -> Vec<Slot> {
        vec![Slot { slot: 0, name: "Telescope".into(), device: "/dev/video9".into() }]
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }
    fn open(&self, _slot: u32, width: u32, height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Ok(Box::new(Cam(self.0.clone(), (width, height))))
    }
}

/// Mirrors each frame, like the Python test's frame_pipeline, and counts them.
struct Flip(Arc<AtomicUsize>);

impl CpuStep for Flip {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        assert_eq!(frame.format, PixelFormat::I420);
        self.0.fetch_add(1, Ordering::SeqCst);
        let (w, h) = (frame.width as usize, frame.height as usize);
        let (y, chroma) = frame.data_mut().split_at_mut(w * h);
        for row in y.chunks_mut(w).chain(chroma.chunks_mut(w / 2)) {
            row.reverse();
        }
    }
}

/// The grey picture of each frame of the stream, mirrored.
fn mirrored_pictures(units: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut dec = FfmpegMedia.decoder(Codec::H264, &DecodeOptions { hardware: false }).expect("an H.264 decoder");
    let mut pictures = Vec::new();
    for unit in units {
        if let Some(frame) = dec.decode(&Packet { data: [unit.as_slice(), AUD].concat(), time_ns: 0 }).newest {
            let w = frame.width as usize;
            pictures.push(frame.data[..w * frame.height as usize].chunks(w).flat_map(|row| row.iter().rev().copied()).collect());
        }
    }
    pictures
}

fn gap(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as f64).sum::<f64>() / a.len() as f64
}

#[test]
fn stream_worker_sends_browser_frames_to_the_virtual_camera() {
    let Some(units) = telescope_media::test_h264_moving(30, SIZE.0, SIZE.1, 5, "baseline") else {
        eprintln!("this build has no H.264 encoder: skipped");
        return;
    };
    let pictures = mirrored_pictures(&units);
    let feed = Arc::new(BrowserFeed::new());
    let gen = feed.connect();
    feed.note_rtc(gen, "H264", "");

    let sent: Arc<Mutex<Vec<Frame>>> = Arc::default();
    let flipped = Arc::new(AtomicUsize::new(0));
    let reader = feed.clone();
    let opener: Opener = Arc::new(move || {
        let (codec, video) = BrowserVideo::open(reader.clone()).ok_or("no browser")?;
        Ok((codec, Box::new(video) as Box<dyn VideoFeed>).into())
    });
    let setup = WorkerSetup {
        url: "browser:".into(), opener, width: None, height: None, fps: 30, canvas: (None, None), slot: 0, source_id: "browser".into(),
        hardware_decode: false, plugin_source: true, waiting_text: None, vision: None, gate: None,
        steps: vec![StagedStep { owner: "flip".into(), stage: FrameStage::Effects, step: FrameStep::Cpu(Box::new(Flip(flipped.clone()))) }],
    };
    let log: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let events: Events = {
        let log = log.clone();
        Arc::new(move |e| {
            if let WorkerEvent::Status(kind, text) = e {
                log.lock().unwrap().push((kind, text));
            }
        })
    };
    let slot: Arc<dyn FrameSlot> = FfmpegMedia.stream_frames().expect("frames").new_slot();
    let worker = StreamWorker::start(setup, Arc::new(FfmpegMedia), Arc::new(Cams(sent.clone())), slot, events).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let pumper = {
        let (stop, feed) = (stop.clone(), feed.clone());
        std::thread::spawn(move || {
            for unit in units.iter().cycle() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                feed.put_video(gen, Codec::H264, [unit.as_slice(), AUD].concat(), 1000, 0);
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let arrived = wait(|| sent.lock().unwrap().len() >= 5);
    worker.stop();
    stop.store(true, Ordering::SeqCst);
    pumper.join().unwrap();
    assert!(arrived, "{:?}", log.lock().unwrap());

    let last = sent.lock().unwrap().last().unwrap().clone();
    assert_eq!((last.width, last.height), SIZE);
    assert_eq!(feed.frame_bytes(), 1000);
    assert!(flipped.load(Ordering::SeqCst) >= 5);
    // The picture that came out is one the browser sent, through the step
    let y = &last.data[..(SIZE.0 * SIZE.1) as usize];
    let nearest = pictures.iter().map(|p| gap(y, p)).fold(f64::MAX, f64::min);
    assert!(nearest < 6.0, "{nearest} levels from the nearest frame sent");
    assert!(log.lock().unwrap().iter().any(|(kind, _)| kind == "idle"), "the worker's thread ended");
}

struct Written(Arc<Mutex<Vec<i16>>>);

impl MicSink for Written {
    fn write(&mut self, samples: &[i16]) -> std::io::Result<()> {
        self.0.lock().unwrap().extend_from_slice(samples);
        std::thread::sleep(Duration::from_millis(5));
        Ok(())
    }
}

#[test]
fn audio_worker_plays_the_browsers_mic() {
    let feed = Arc::new(BrowserFeed::new());
    let gen = feed.connect();
    let written: Arc<Mutex<Vec<i16>>> = Arc::default();
    let statuses: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let reader = feed.clone();
    let opener: telescope_audio::worker::Opener = Arc::new(move || Ok(Box::new(reader.open_audio()?) as Box<dyn AudioFeed>));
    let sink = written.clone();
    let open_sink: OpenSink = Arc::new(move || Ok(Box::new(Written(sink.clone())) as Box<dyn MicSink>));
    let told = statuses.clone();
    let mut worker = AudioWorker::new("browser:/audio", opener, open_sink, Arc::new(move |kind, text| told.lock().unwrap().push((kind.into(), text.into()))));
    worker.start();

    // The mic asks for audio: the page hears it should send some
    assert!(wait(|| feed.page_config().1["audio"] == true));
    let tone: Vec<u8> = (0..4800).flat_map(|i| (((i as f64 / 10.0).sin() * 10000.0) as i16).to_le_bytes()).collect();
    for chunk in tone.chunks(1920) {
        feed.put_audio(gen, chunk);
    }
    let played = wait(|| written.lock().unwrap().iter().any(|s| *s != 0));
    worker.stop();
    assert!(played, "{:?}", statuses.lock().unwrap());
    assert!(statuses.lock().unwrap().contains(&("ok".to_string(), String::new())), "{:?}", statuses.lock().unwrap());
}
