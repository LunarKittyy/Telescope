//! test_browser_rtc.py's real connection: another str0m peer stands in for the browser and sends this build's H.264 and Opus. They go through the
//! real session, BrowserFeed and BrowserVideo into the real decoders, so what's checked is a picture and samples, not packets.

use std::net::{IpAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::change::SdpAnswer;
use str0m::format::Codec as RtcCodec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, RtcConfig};
use telescope_core::frame::PixelFormat;
use telescope_core::media::{AudioDecoder, DecodeOptions, Media, VideoFeed};
use telescope_link::browser::feed::{BrowserFeed, BrowserVideo};
use telescope_link::browser::rtc::{RtcSession, StrSession};
use telescope_media::FfmpegMedia;

const SIZE: (u32, u32) = (320, 240);
const PEAK: f64 = 8000.0;

/// A str0m peer on loopback sending a moving grey picture and a tone every 20 ms, the way the page sends its camera and mic; it runs until stop.
fn fake_browser(video_units: Vec<Vec<u8>>, mic_packets: Vec<Vec<u8>>, answered: Receiver<String>, stop: Arc<AtomicBool>) -> String {
    let mut rtc = RtcConfig::new().set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags())).build(Instant::now());
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let local = socket.local_addr().unwrap();
    rtc.add_local_candidate(Candidate::host(local, "udp").unwrap());
    let mut change = rtc.sdp_api();
    let video = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let audio = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let sdp = offer.to_sdp_string();
    std::thread::spawn(move || {
        let answer = answered.recv().unwrap();
        rtc.sdp_api().accept_answer(pending, SdpAnswer::from_sdp_string(&answer).unwrap()).unwrap();
        let mut buf = vec![0u8; 2000];
        let (mut connected, mut next, mut n) = (false, Instant::now(), 0usize);
        while !stop.load(Ordering::SeqCst) {
            if connected && Instant::now() >= next {
                next = Instant::now() + Duration::from_millis(20);
                let sends = [(video, RtcCodec::H264, &video_units[n % video_units.len()], Frequency::NINETY_KHZ),
                             (audio, RtcCodec::Opus, &mic_packets[n % mic_packets.len()], Frequency::FORTY_EIGHT_KHZ)];
                n += 1;
                for (mid, codec, data, rate) in sends {
                    let Some(writer) = rtc.writer(mid) else { continue };
                    let Some(pt) = writer.payload_params().find(|p| p.spec().codec == codec).map(|p| p.pt()) else { continue };
                    let ticks = n as u64 * u64::from(rate.get()) / 50;
                    let _ = writer.write(pt, Instant::now(), MediaTime::new(ticks, rate), data.clone());
                }
            }
            let timeout = match rtc.poll_output() {
                Ok(Output::Timeout(t)) => t,
                Ok(Output::Transmit(t)) => {
                    let _ = socket.send_to(&t.contents, t.destination);
                    continue;
                }
                Ok(Output::Event(Event::Connected)) => {
                    connected = true;
                    continue;
                }
                Ok(Output::Event(_)) => continue,
                Err(_) => return,
            };
            let wait = timeout.saturating_duration_since(Instant::now()).min(Duration::from_millis(5)).max(Duration::from_millis(1));
            socket.set_read_timeout(Some(wait)).unwrap();
            let input = match socket.recv_from(&mut buf) {
                Ok((got, source)) => Input::Receive(Instant::now(), Receive {
                    proto: Protocol::Udp, source, destination: local, contents: buf[..got].try_into().unwrap(),
                }),
                Err(_) => Input::Timeout(Instant::now()),
            };
            if rtc.handle_input(input).is_err() {
                return;
            }
        }
    });
    sdp
}

// test_browser_rtc.py::test_a_browser_over_webrtc_reaches_the_reader_and_the_mic
#[test]
fn a_browser_over_webrtc_reaches_the_reader_and_the_mic() {
    let greys: Vec<u8> = (0..50).map(|n| 100 + n).collect();
    let (Some(units), Some(packets)) = (telescope_media::test_h264_grey(&greys, SIZE.0, SIZE.1, 10), telescope_media::test_opus_tone(50, PEAK)) else {
        eprintln!("this build has no H.264 or Opus encoder: skipped");
        return;
    };
    let (tell, answered) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let offer = fake_browser(units, packets, answered, stop.clone());

    let feed = Arc::new(BrowserFeed::new());
    let gen = feed.connect();
    let mic = feed.open_audio().unwrap();
    let (video, audio) = (feed.clone(), feed.clone());
    let loopback: Vec<IpAddr> = vec![[127, 0, 0, 1].into()];
    let opus = Arc::new(|| FfmpegMedia.opus_decoder().ok() as Option<Box<dyn AudioDecoder>>);
    let mut session = StrSession::new(loopback, opus, Box::new(move |codec, data, size| video.put_video(gen, codec, data, size)),
                                      Box::new(move |pcm| audio.put_audio(gen, &pcm)));
    let answer = session.answer(&offer, 4000).unwrap();
    assert!(answer.contains("x-google-start-bitrate") && ["H264", "VP8"].contains(&session.codec().as_str()), "{}", session.codec());
    feed.note_rtc(gen, &session.codec(), "");
    tell.send(answer).unwrap();

    // The reader's side: the stream opens the feed and decodes what it reads
    let (codec, mut reader) = BrowserVideo::open(feed.clone()).expect("the browser is there");
    let mut decoder = FfmpegMedia.decoder(codec, &DecodeOptions { hardware: false }).expect("a decoder");
    let deadline = Instant::now() + Duration::from_secs(10);
    let frame = loop {
        assert!(Instant::now() < deadline, "no picture came out");
        let packet = reader.read().expect("frames keep coming");
        if let Some(frame) = decoder.decode(&packet).newest {
            break frame;
        }
    };
    assert_eq!((frame.width, frame.height, frame.format), (SIZE.0, SIZE.1, PixelFormat::I420));
    let (y, chroma) = frame.data.split_at((SIZE.0 * SIZE.1) as usize);
    let mean = |plane: &[u8]| plane.iter().map(|v| f64::from(*v)).sum::<f64>() / plane.len() as f64;
    // Limited-range luma back to the 0..255 the Python test's BGR frame is in
    let brightness = (mean(y) - 16.0) * 255.0 / 219.0;
    assert!((90.0..=160.0).contains(&brightness), "{brightness}");
    assert!((mean(chroma) - 128.0).abs() < 2.0, "a grey picture stays grey: {}", mean(chroma));

    // On a thread of its own, since the mic's read blocks for as long as it's open
    let (got, heard) = channel();
    std::thread::spawn(move || {
        let mut pcm = Vec::new();
        while pcm.len() < 9600 { // 100 ms of 48 kHz mono s16le
            let more = mic.read_some(4096);
            if more.is_empty() {
                break;
            }
            pcm.extend(more);
        }
        let _ = got.send(pcm);
    });
    let pcm = heard.recv_timeout(Duration::from_secs(10)).expect("the mic's samples");
    stop.store(true, Ordering::SeqCst);
    session.close();
    let samples: Vec<i16> = pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    assert!(samples.len() >= 4800, "{} samples", samples.len());
    let loudest = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(loudest > 1000, "loudest sample {loudest}");
}
