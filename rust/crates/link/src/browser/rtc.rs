//! The Browser camera's WebRTC side (browser_rtc.py): answers a page's offer and hands its video and mic on.
//!
//! str0m does the WebRTC on UDP sockets and a thread of its own per browser. Python's aiortc decoded the video there;
//! here the frames go on still encoded (H.264 as Annex B, or VP8), so the stream decodes them like a phone's, on the
//! graphics card when it can. The mic arrives as Opus and goes on as 48 kHz mono s16le, the same PCM the phone's
//! /v1/audio sends.
//!
//! Nothing here reaches outside the local network: no STUN or TURN server is set, so only the two devices' own
//! addresses are tried.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::bwe::Bitrate;
use str0m::change::SdpOffer;
use str0m::format::Codec as RtcCodec;
use str0m::media::{KeyframeRequestKind, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use telescope_core::media::{AudioDecoder, Codec};

/// aiortc's 128 packets never completed a 1080p keyframe; str0m reorders this many before it gives a frame up.
pub const JITTER_PACKETS: usize = 1024;
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the bytes behind each frame are worked out, for the throughput readout.
const STATS_EVERY: Duration = Duration::from_secs(1);
/// What the page is asked to prefer, in order: what this computer decodes.
pub const VIDEO_CODECS: [&str; 2] = ["H264", "VP8"];
/// The longest the connection's thread sleeps, so close() is noticed soon.
const TICK: Duration = Duration::from_millis(100);
/// How long the thread sleeps when no socket has anything.
const IDLE_NAP: Duration = Duration::from_millis(2);
/// A keyframe asked for again no sooner than this.
const KEYFRAME_AGAIN: Duration = Duration::from_secs(1);
/// How often the sender hears what it may use. Firefox lowers its size without this and never climbs back.
const REMB_EVERY: Duration = Duration::from_secs(1);

/// The offer couldn't be answered; the text is for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtcError(pub String);

// ── SDP ──────────────────────────────────────────────────────────────────────

/// The `a=rtpmap:<pt> <name>/` of a line: (pt, name).
fn rtpmap(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("a=rtpmap:")?;
    let (pt, rest) = rest.split_once(' ')?;
    let (name, _) = rest.split_once('/')?;
    (!pt.is_empty() && pt.bytes().all(|b| b.is_ascii_digit()) && !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then_some((pt, name))
}

/// The video codec an answer settled on ("H264", "VP8"), or "" when it has none.
pub fn answer_codec(sdp: &str) -> String {
    let mut in_video = false;
    for line in sdp.lines() {
        if line.starts_with("m=") {
            in_video = line.starts_with("m=video") && !line.starts_with("m=video 0 ");
        } else if in_video {
            if let Some((_, name)) = rtpmap(line) {
                let name = name.to_ascii_uppercase();
                if VIDEO_CODECS.contains(&name.as_str()) {
                    return name;
                }
            }
        }
    }
    String::new()
}

/// Tells Chrome to start near max_kbps instead of 300 kbps (it would take half a minute to get there) and not to go
/// over it. Browsers that don't know these parameters ignore them.
pub fn tune_answer(sdp: &str, max_kbps: u32) -> String {
    let start = 300.max(max_kbps * 2 / 3);
    let extra = format!("x-google-start-bitrate={start};x-google-max-bitrate={max_kbps}");
    let lines: Vec<&str> = sdp.split("\r\n").collect();
    let pts: Vec<&str> = lines.iter().filter_map(|l| rtpmap(l))
        .filter(|(_, name)| VIDEO_CODECS.contains(&name.to_ascii_uppercase().as_str())).map(|(pt, _)| pt).collect();
    let has_fmtp = |pt: &str| lines.iter().any(|l| l.starts_with(&format!("a=fmtp:{pt} ")));
    let mut out = Vec::new();
    for line in &lines {
        let mut line = line.to_string();
        let kind_pt = line.strip_prefix("a=").and_then(|r| {
            let (kind, rest) = r.split_once(':')?;
            let (pt, _) = rest.split_once(' ')?;
            Some((kind.to_string(), pt.to_string()))
        });
        let ours = kind_pt.as_ref().filter(|(kind, pt)| (kind == "rtpmap" || kind == "fmtp") && pts.contains(&pt.as_str()));
        if let Some((kind, _)) = ours {
            if kind == "fmtp" {
                line = format!("{line};{extra}");
            }
        }
        out.push(line);
        if let Some((kind, pt)) = ours {
            if kind == "rtpmap" && !has_fmtp(pt) {
                out.push(format!("a=fmtp:{pt} {extra}"));
            }
        }
    }
    out.join("\r\n")
}

/// The bitrate a browser's camera may use on the local network at this size and rate.
pub fn max_kbps_for(width: u32, height: u32, fps: u32) -> u32 {
    let kbps = (f64::from(width) * f64::from(height) * f64::from(fps) * 0.15 / 1000.0) as u32;
    kbps.clamp(1500, 12_000)
}

// ── A browser's connection ───────────────────────────────────────────────────

/// One encoded video frame from the page: (codec, Annex B H.264 or a VP8 frame, what each frame took on the network
/// lately). Called on the connection's thread, so it must only hand the data on.
pub type VideoSink = Box<dyn FnMut(Codec, Vec<u8>, u64) + Send>;
/// 20 ms or so of the page's mic as 48 kHz mono s16le, on the connection's thread.
pub type AudioSink = Box<dyn FnMut(Vec<u8>) + Send>;

/// One page's peer connection, however it's made (tests stand one in).
pub trait RtcSession: Send {
    /// The answer to a complete (non-trickle) offer; blocks up to ANSWER_TIMEOUT.
    fn answer(&mut self, offer: &str, max_kbps: u32) -> Result<String, RtcError>;
    /// The video codec the answer settled on: "H264" or "VP8".
    fn codec(&self) -> String;
    /// Ends the connection; returns at once.
    fn close(&mut self);
    /// A keyframe, soon: the stream lost its place.
    fn want_keyframe(&self) {}
}

/// Makes a browser's WebRTC side for each offer.
pub type RtcMaker = Arc<dyn Fn(VideoSink, AudioSink) -> Box<dyn RtcSession> + Send + Sync>;

/// The real one: str0m on a UDP socket per address of this computer, each one offered as a host candidate.
pub struct StrSession {
    addresses: Vec<IpAddr>,
    opus: Arc<dyn Fn() -> Option<Box<dyn AudioDecoder>> + Send + Sync>,
    sinks: Option<(VideoSink, AudioSink)>,
    codec: String,
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
}

impl StrSession {
    /// `addresses` are the ones the browser may reach this computer on; `opus` makes the mic's decoder.
    pub fn new(addresses: Vec<IpAddr>, opus: Arc<dyn Fn() -> Option<Box<dyn AudioDecoder>> + Send + Sync>, video: VideoSink,
               audio: AudioSink) -> StrSession {
        StrSession { addresses, opus, sinks: Some((video, audio)), codec: String::new(), stop: Arc::default(), keyframe: Arc::default() }
    }

    fn config() -> RtcConfig {
        RtcConfig::new()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .clear_codecs()
            .enable_h264(true)
            .enable_vp8(true)
            .enable_opus(true, false)
            .set_reordering_size_video(JITTER_PACKETS)
    }
}

impl RtcSession for StrSession {
    fn answer(&mut self, offer: &str, max_kbps: u32) -> Result<String, RtcError> {
        let Some((video, audio)) = self.sinks.take() else { return Err(RtcError("answered already".into())) };
        if self.stop.load(Ordering::SeqCst) {
            return Err(RtcError("closed".into()));
        }
        let offer = SdpOffer::from_sdp_string(offer).map_err(|e| RtcError(format!("not an offer: {e}")))?;
        let mut rtc = StrSession::config().build(Instant::now());
        // One socket per address rather than one on all of them, so each packet's own address is known: str0m
        // matches it against the candidates
        let mut sockets = Vec::new();
        for ip in &self.addresses {
            let Ok(socket) = UdpSocket::bind(SocketAddr::new(*ip, 0)) else { continue };
            let Ok(local) = socket.local_addr() else { continue };
            if let Ok(c) = Candidate::host(local, "udp") {
                rtc.add_local_candidate(c);
                let _ = socket.set_nonblocking(true);
                sockets.push(socket);
            }
        }
        if sockets.is_empty() {
            return Err(RtcError("no address to offer the browser".into()));
        }
        let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| RtcError(e.to_string()))?;
        let sdp = answer.to_sdp_string();
        self.codec = answer_codec(&sdp);
        if self.codec.is_empty() {
            return Err(RtcError("the browser offered no video codec this computer decodes".into()));
        }
        log::info!("Browser camera: WebRTC answered with {}", self.codec);
        let (stop, keyframe, opus) = (self.stop.clone(), self.keyframe.clone(), self.opus.clone());
        let (started, ready) = mpsc::channel();
        std::thread::Builder::new().name("browser-rtc".into()).spawn(move || {
            let _ = started.send(());
            run(rtc, &sockets, &stop, &keyframe, video, audio, opus.as_ref()(), max_kbps);
        }).map_err(|e| RtcError(e.to_string()))?;
        ready.recv_timeout(ANSWER_TIMEOUT).map_err(|_| RtcError("the connection's thread didn't start".into()))?;
        Ok(tune_answer(&sdp, max_kbps))
    }

    fn codec(&self) -> String {
        self.codec.clone()
    }

    fn close(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    fn want_keyframe(&self) {
        self.keyframe.store(true, Ordering::SeqCst);
    }
}

impl Drop for StrSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// The connection until it's closed or the browser goes.
fn run(mut rtc: Rtc, sockets: &[UdpSocket], stop: &AtomicBool, keyframe: &AtomicBool, mut video: VideoSink, mut audio: AudioSink,
       mut opus: Option<Box<dyn AudioDecoder>>, max_kbps: u32) {
    let mut buf = vec![0u8; 2000];
    let mut video_mid: Option<Mid> = None;
    let (mut frames, mut received, mut since, mut per_frame) = (0u64, 0u64, Instant::now(), 0u64);
    let mut asked = Instant::now() - KEYFRAME_AGAIN;
    let mut told = Instant::now() - REMB_EVERY;
    while !stop.load(Ordering::SeqCst) && rtc.is_alive() {
        if keyframe.swap(false, Ordering::SeqCst) && asked.elapsed() >= KEYFRAME_AGAIN {
            if let Some(mut writer) = video_mid.and_then(|mid| rtc.writer(mid)) {
                asked = Instant::now();
                let _ = writer.request_keyframe(None, KeyframeRequestKind::Pli);
            }
        }
        if told.elapsed() >= REMB_EVERY {
            if let Some(mid) = video_mid {
                if let Some(stream) = rtc.direct_api().stream_rx_by_mid(mid, None) {
                    told = Instant::now();
                    stream.request_remb(Bitrate::kbps(u64::from(max_kbps)));
                }
            }
        }
        let timeout = match rtc.poll_output() {
            Ok(Output::Timeout(at)) => at,
            Ok(Output::Transmit(t)) => {
                if let Some(socket) = sockets.iter().find(|s| s.local_addr().ok() == Some(t.source)) {
                    let _ = socket.send_to(&t.contents, t.destination);
                }
                continue;
            }
            Ok(Output::Event(event)) => {
                match event {
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return,
                    Event::MediaAdded(added) if added.kind == MediaKind::Video => video_mid = Some(added.mid),
                    Event::MediaData(data) => match data.params.spec().codec {
                        RtcCodec::H264 | RtcCodec::Vp8 => {
                            let codec = if data.params.spec().codec == RtcCodec::H264 { Codec::H264 } else { Codec::Vp8 };
                            if !data.contiguous {
                                keyframe.store(true, Ordering::SeqCst); // a gap: the frames after it need a keyframe
                            }
                            frames += 1;
                            if since.elapsed() >= STATS_EVERY {
                                per_frame = received.checked_div(frames).unwrap_or(per_frame);
                                (frames, received, since) = (0, 0, Instant::now());
                            }
                            video(codec, data.data.to_vec(), per_frame);
                        }
                        RtcCodec::Opus => {
                            if let Some(decoder) = opus.as_mut() {
                                let pcm = decoder.decode(&data.data);
                                if !pcm.is_empty() {
                                    audio(pcm);
                                }
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
                continue;
            }
            Err(e) => {
                log::debug!("Browser camera: the WebRTC connection ended: {e}");
                return;
            }
        };
        let wait = timeout.saturating_duration_since(Instant::now()).min(TICK);
        if wait.is_zero() {
            if rtc.handle_input(Input::Timeout(Instant::now())).is_err() {
                return;
            }
            continue;
        }
        let got = sockets.iter().find_map(|socket| {
            let (n, source) = socket.recv_from(&mut buf).ok()?;
            Some((n, source, socket.local_addr().ok()?))
        });
        let input = match got {
            Some((n, source, destination)) => {
                received += n as u64;
                let Ok(contents) = buf[..n].try_into() else { continue };
                Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination, contents })
            }
            None => {
                // Nothing in on any of them: a short nap, never past str0m's own next deadline
                std::thread::sleep(wait.min(IDLE_NAP));
                Input::Timeout(Instant::now())
            }
        };
        if rtc.handle_input(input).is_err() {
            return;
        }
    }
}
