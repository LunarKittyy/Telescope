//! tests/test_browser_rtc.py, test for test: the SDP helpers, and a real connection with another str0m peer standing in
//! for the browser.

use super::rtc::*;
use std::net::{IpAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use str0m::change::SdpAnswer;
use str0m::format::Codec as RtcCodec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, RtcConfig};
use telescope_core::media::{AudioDecoder, Codec};

const ANSWER: &str = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102 103 96\r\n\
a=rtpmap:102 H264/90000\r\na=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 rtx/90000\r\na=fmtp:103 apt=102\r\n\
a=rtpmap:96 VP8/90000\r\n";

#[test]
fn answer_codec_is_the_first_video_codec() {
    assert_eq!(answer_codec(ANSWER), "H264");
    assert_eq!(answer_codec(&ANSWER.replace("H264/", "AV1/")), "VP8");
    assert_eq!(answer_codec(&ANSWER.replace("m=video 9 ", "m=video 0 ")), ""); // rejected
    assert_eq!(answer_codec("m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2"), "");
}

#[test]
fn tune_answer_sets_the_bitrates_on_video_codecs_only() {
    let out = tune_answer(ANSWER, 6000);
    let lines: Vec<&str> = out.split("\r\n").collect();
    assert!(lines.contains(&"a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f;\
x-google-start-bitrate=4000;x-google-max-bitrate=6000"));
    let vp8 = lines.iter().position(|l| *l == "a=rtpmap:96 VP8/90000").unwrap();
    assert_eq!(lines[vp8 + 1], "a=fmtp:96 x-google-start-bitrate=4000;x-google-max-bitrate=6000");
    assert!(lines.contains(&"a=fmtp:111 minptime=10;useinbandfec=1") && lines.contains(&"a=fmtp:103 apt=102"));
    assert_eq!(*lines.last().unwrap(), ""); // still ends in a line break
}

#[test]
fn max_kbps_follows_size_and_rate_within_bounds() {
    assert_eq!(max_kbps_for(1920, 1080, 30), 9331);
    assert_eq!(max_kbps_for(854, 480, 15), 1500);
    assert_eq!(max_kbps_for(3840, 2160, 60), 12_000);
}

// ── A real connection ────────────────────────────────────────────────────────

/// Hands the packet on as it came, so the test sees what the browser sent.
struct Echo;

impl AudioDecoder for Echo {
    fn decode(&mut self, packet: &[u8]) -> Vec<u8> {
        packet.to_vec()
    }
}

/// A str0m peer on loopback sending a frame of H.264 and a packet of Opus now and then, the way the page sends its
/// camera and mic; it runs until stop.
fn fake_browser(answered: std::sync::mpsc::Receiver<String>, stop: Arc<AtomicBool>) -> String {
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
        let (mut connected, mut next, mut n) = (false, Instant::now(), 0u64);
        while !stop.load(Ordering::SeqCst) {
            if connected && Instant::now() >= next {
                next = Instant::now() + Duration::from_millis(20);
                n += 1;
                for (mid, codec, data, rate) in [(video, RtcCodec::H264, vec![0, 0, 0, 1, 0x65, 0x88, n as u8], Frequency::NINETY_KHZ),
                                                 (audio, RtcCodec::Opus, vec![0xF8, 0xFF, 0xFE, n as u8], Frequency::FORTY_EIGHT_KHZ)] {
                    let Some(writer) = rtc.writer(mid) else { continue };
                    let Some(pt) = writer.payload_params().find(|p| p.spec().codec == codec).map(|p| p.pt()) else { continue };
                    let ticks = n * u64::from(rate.get()) / 50;
                    let _ = writer.write(pt, Instant::now(), MediaTime::new(ticks, rate), data);
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

#[test]
fn a_browser_over_webrtc_reaches_the_reader_and_the_mic() {
    let (tell, answered) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let offer = fake_browser(answered, stop.clone());
    let frames: Arc<Mutex<Vec<(Codec, Vec<u8>)>>> = Arc::default();
    let heard: Arc<Mutex<Vec<u8>>> = Arc::default();
    let (f, h) = (frames.clone(), heard.clone());
    let loopback: Vec<IpAddr> = vec![[127, 0, 0, 1].into()];
    let mut session = StrSession::new(loopback, Arc::new(|| Some(Box::new(Echo) as Box<dyn AudioDecoder>)),
                                      Box::new(move |codec, data, _| f.lock().unwrap().push((codec, data))),
                                      Box::new(move |pcm| h.lock().unwrap().extend(pcm)));
    let answer = session.answer(&offer, 4000).unwrap();
    assert!(answer.contains("x-google-start-bitrate") && session.codec() == "H264");
    tell.send(answer).unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while (frames.lock().unwrap().len() < 3 || heard.lock().unwrap().len() < 12) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::SeqCst);
    session.close();
    let frames = frames.lock().unwrap();
    assert!(frames.len() >= 3, "{} frames", frames.len());
    let (codec, data) = &frames[0];
    assert_eq!(*codec, Codec::H264);
    assert!(data.starts_with(&[0, 0, 0, 1, 0x65, 0x88]), "{data:?}");
    let heard = heard.lock().unwrap();
    assert!(heard.len() >= 12 && heard.starts_with(&[0xF8, 0xFF, 0xFE]), "{heard:?}");
}

#[test]
fn an_offer_without_a_codec_this_computer_decodes_is_refused() {
    let mut session = StrSession::new(vec![], Arc::new(|| None), Box::new(|_, _, _| {}), Box::new(|_| {}));
    assert!(session.answer("v=0\r\nnot an offer\r\n", 4000).is_err());
}
