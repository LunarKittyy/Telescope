//! test_browser_rtc.py's real connection: another str0m peer stands in for the browser and sends this build's H.264 and Opus. They go through the
//! real session, BrowserFeed and BrowserVideo into the real decoders, so what's checked is a picture and samples, not packets.

use std::net::{IpAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use str0m::bwe::BweKind;
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
    fake_browser_counting(Arc::new(Mutex::new(video_units)), mic_packets, answered, stop, Arc::default(), Arc::default())
}

/// The same, sending whatever video_units holds now, counting the keyframes it's asked for and keeping the last bitrate (kbps) it's told to stay under.
fn fake_browser_counting(video_units: Arc<Mutex<Vec<Vec<u8>>>>, mic_packets: Vec<Vec<u8>>, answered: Receiver<String>, stop: Arc<AtomicBool>,
                         asked: Arc<AtomicUsize>, told: Arc<AtomicUsize>) -> String {
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
                let unit = { let units = video_units.lock().unwrap(); units[n % units.len()].clone() };
                let sends = [(video, RtcCodec::H264, &unit, Frequency::NINETY_KHZ),
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
                Ok(Output::Event(Event::KeyframeRequest(_))) => {
                    asked.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
                Ok(Output::Event(Event::EgressBitrateEstimate(BweKind::Remb { estimate, .. }))) => {
                    told.store((estimate.as_f64() / 1000.0) as usize, Ordering::SeqCst);
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

/// Firefox on a phone sent in bursts to anything else, and a browser takes the answer's first entry, whose plain Baseline VA-API refuses.
#[test]
fn the_answer_matches_what_browsers_send_evenly_to() {
    let (_tell, answered) = channel::<String>();
    let offer = fake_browser(vec![vec![0]], vec![vec![0]], answered, Arc::new(AtomicBool::new(true)));
    let mut session = StrSession::new(vec![[127, 0, 0, 1].into()], Arc::new(|| None), Box::new(|_, _, _, _| {}), Box::new(|_| {}));
    let answer = session.answer(&offer, 4000).unwrap();
    session.close();
    let profiles: Vec<&str> = answer.lines().filter_map(|l| l.split("profile-level-id=").nth(1)).map(|p| &p[..6]).collect();
    assert_eq!(profiles, ["42e01f", "42001f"], "{answer}");
    assert!(!answer.contains("transport-cc") && !answer.contains("video-orientation"), "{answer}");
    assert!(answer.contains("goog-remb"), "{answer}");
}

/// Firefox has no transport-cc feedback to go on, and aiortc told it what it may use: it lowered its rate and climbed back slowly without that.
#[test]
fn the_sender_is_told_what_it_may_use() {
    let (tell, answered) = channel();
    let (stop, told) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let offer = fake_browser_counting(Arc::new(Mutex::new(vec![vec![0; 100]])), vec![vec![0]], answered, stop.clone(), Arc::default(), told.clone());
    let loopback: Vec<IpAddr> = vec![[127, 0, 0, 1].into()];
    let mut session = StrSession::new(loopback, Arc::new(|| None), Box::new(|_, _, _, _| {}), Box::new(|_| {}));
    tell.send(session.answer(&offer, 4000).unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while told.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(told.load(Ordering::SeqCst), 4000);
    session.close();
    stop.store(true, Ordering::SeqCst);
}

/// The decoder changed mid-stream and has nothing to start from until a keyframe comes, and WebRTC doesn't resend the first one.
#[test]
fn a_keyframe_is_asked_for_until_one_comes() {
    let greys: Vec<u8> = (0..30).map(|n| 100 + n).collect();
    let (Some(units), Some(packets)) = (telescope_media::test_h264(&greys, SIZE.0, SIZE.1, 300, false), telescope_media::test_opus_tone(10, PEAK)) else {
        eprintln!("this build has no H.264 or Opus encoder: skipped");
        return;
    };
    let keyframe = units[0].clone();
    let others = Arc::new(Mutex::new(units[1..].to_vec()));
    let (tell, answered) = channel();
    let (stop, asked) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let offer = fake_browser_counting(others.clone(), packets, answered, stop.clone(), asked.clone(), Arc::default());
    let feed = Arc::new(BrowserFeed::new());
    let gen = feed.connect();
    let (video, audio) = (feed.clone(), feed.clone());
    let opus = Arc::new(|| FfmpegMedia.opus_decoder().ok() as Option<Box<dyn AudioDecoder>>);
    let loopback: Vec<IpAddr> = vec![[127, 0, 0, 1].into()];
    let mut session = StrSession::new(loopback, opus, Box::new(move |codec, data, size, made| video.put_video(gen, codec, data, size, made)),
                                      Box::new(move |pcm| audio.put_audio(gen, &pcm)));
    // Before the connection is up, when no request can leave yet
    session.want_keyframe();
    tell.send(session.answer(&offer, 4000).unwrap()).unwrap();
    let until = |wanted: usize| {
        let deadline = Instant::now() + Duration::from_secs(8);
        while asked.load(Ordering::SeqCst) < wanted && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        asked.load(Ordering::SeqCst)
    };
    assert!(until(3) >= 3, "asked {} times with no keyframe coming", asked.load(Ordering::SeqCst));
    *others.lock().unwrap() = vec![keyframe];
    std::thread::sleep(Duration::from_millis(300));
    let settled = asked.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(asked.load(Ordering::SeqCst), settled, "the keyframe came, so the asking stops");
    session.close();
    stop.store(true, Ordering::SeqCst);
}

// test_browser_rtc.py::test_a_browser_over_webrtc_reaches_the_reader_and_the_mic
#[test]
fn a_browser_over_webrtc_reaches_the_reader_and_the_mic() {
    let greys: Vec<u8> = (0..50).map(|n| 100 + n).collect();
    let (Some(units), Some(packets)) = (telescope_media::test_h264(&greys, SIZE.0, SIZE.1, 10, false), telescope_media::test_opus_tone(50, PEAK)) else {
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
    let mut session = StrSession::new(loopback, opus, Box::new(move |codec, data, size, made| video.put_video(gen, codec, data, size, made)),
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
    let samples: Vec<i16> = pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
    assert!(samples.len() >= 4800, "{} samples", samples.len());
    let loudest = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(loudest > 1000, "loudest sample {loudest}");
}
