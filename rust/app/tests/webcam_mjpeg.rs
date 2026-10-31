// A webcam's JPEG from the reader through the stream's real decoder (test_webcams.py's test_mjpeg_frames_are_decoded_by_the_streams_decoders).
// Here because outputs may not use media: they only meet in the window.
use std::sync::{Arc, Mutex};
use telescope_core::media::{Codec, DecodeOptions, FeedStopper, Media, VideoFeed};
use telescope_outputs::webcam_capture::{is_jpeg, open, Capture, ReaderDeps};
use telescope_outputs::webcams::{Choice, Device};

struct OneFrame(Vec<Vec<u8>>);

struct NoStop;

impl FeedStopper for NoStop {
    fn stop(&self) {}
}

impl Capture for OneFrame {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        self.0.pop().ok_or_else(|| "nothing".to_string())
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(NoStop)
    }
}

#[test]
fn mjpeg_frames_are_decoded_by_the_streams_decoders() {
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode(&vec![200u8; 1280 * 720 * 3], 1280, 720, image::ExtendedColorType::Rgb8).unwrap();
    let frames = Mutex::new(Some(vec![jpeg.clone()]));
    let deps = ReaderDeps {
        locate: Arc::new(|| Some(Device { key: "k".into(), name: "HD Webcam".into(), location: "/dev/video0".into(), modes: Vec::new() })),
        want: Arc::new(|_| Some(Choice { fourcc: "MJPG".into(), width: 1280, height: 720, fps: 30.0 })),
        report: Arc::new(|_, _| {}),
        opener: Arc::new(move |_, _| Ok(Box::new(OneFrame(frames.lock().unwrap().take().unwrap_or_default())) as Box<dyn Capture>)),
        users: Arc::new(|_| String::new()),
        exists: Arc::new(|_| true),
    };
    let (choice, mut feed) = open(&deps, &Arc::default()).unwrap();
    assert!(is_jpeg(&choice)); // several JPEGs at once on the stream's decoder threads
    let packet = feed.read().unwrap();
    assert_eq!(packet.data, jpeg, "as it came");
    let mut decoder = telescope_media::FfmpegMedia.decoder(Codec::Mjpeg, &DecodeOptions { hardware: false }).unwrap();
    let frame = decoder.decode(&packet).newest.expect("a picture");
    assert_eq!((frame.width, frame.height), (1280, 720));
    // 200 of 255 on BT.601 limited's luma range
    assert!((frame.data[0] as i32 - 188).abs() < 4, "{}", frame.data[0]);
}
