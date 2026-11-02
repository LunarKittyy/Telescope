//! tests/test_browser_rtc.py, test for test: the SDP helpers here. The real connection needs the decoders too, so it's in app/tests/browser_rtc.rs.

use super::rtc::*;
use std::sync::Arc;

const ANSWER: &str = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102 103 96\r\n\
a=rtpmap:102 H264/90000\r\na=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 rtx/90000\r\na=fmtp:103 apt=102\r\n\
a=rtcp-fb:102 transport-cc\r\na=rtcp-fb:102 nack pli\r\n\
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
    assert!(lines.contains(&"a=rtcp-fb:102 nack pli") && !out.contains("transport-cc"));
}

#[test]
fn max_kbps_follows_size_and_rate_within_bounds() {
    assert_eq!(max_kbps_for(1920, 1080, 30), 9331);
    assert_eq!(max_kbps_for(854, 480, 15), 1500);
    assert_eq!(max_kbps_for(3840, 2160, 60), 12_000);
}

#[test]
fn an_offer_without_a_codec_this_computer_decodes_is_refused() {
    let mut session = StrSession::new(vec![], Arc::new(|| None), Box::new(|_, _, _, _| {}), Box::new(|_| {}));
    assert!(session.answer("v=0\r\nnot an offer\r\n", 4000).is_err());
}
