//! H.264 (Annex-B, in chunks of any size) to I420 (h264_reader.Decoder): on the graphics card when that's on and works,
//! else in software. A card decoder that errors, or takes in pieces of the stream and gives no picture for a moment, is swapped
//! for software on the spot without ending the stream, and not tried again this run. The stream is asked for a keyframe, so the
//! picture resumes right away instead of at the next one the phone was going to send.

use crate::ffmpeg::{Codec, Fault, Forced, Hw};
use crate::packets::{Steps, Tap};
use crate::slow::{self, Timings};
use std::time::Instant;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::frame::Frame;
use telescope_core::media::{Decoded, Decoder, DecodingStatus, Packet, TimingProbe};

/// Card decoders to try, best first: D3D11 works on any Windows card; VA-API covers AMD and Intel on Linux.
#[cfg(windows)]
const HW_TYPES: [Hw; 2] = [Hw::D3d11va, Hw::Cuda];
#[cfg(not(windows))]
const HW_TYPES: [Hw; 2] = [Hw::Vaapi, Hw::Cuda];

/// Pieces of the stream a card decoder may take in a row without a picture before software takes over: half a second at 30 fps.
const SILENT_UNITS: u32 = 15;

/// A card decoder errored this run: software from then on.
static HW_FAILED: AtomicBool = AtomicBool::new(false);
/// When the card gets another go after it was too slow (not for a card that failed): one per run, shared by the streams.
static COOLDOWN: Mutex<slow::Cooldown> = Mutex::new(slow::Cooldown::new());
/// What each live decoder uses ("vaapi", "processor"), counted.
static LIVE: Mutex<BTreeMap<&'static str, usize>> = Mutex::new(BTreeMap::new());

/// The card decoder this computer has, probed once by opening one.
pub fn hardware_device() -> Option<Hw> {
    static FOUND: OnceLock<Option<Hw>> = OnceLock::new();
    *FOUND.get_or_init(|| first_that_opens(|h| Codec::h264(Some(h)).is_ok()))
}

/// The first card decoder that opens, in the order they're tried. One this build or this computer doesn't have just doesn't open.
fn first_that_opens(opens: impl Fn(Hw) -> bool) -> Option<Hw> {
    HW_TYPES.into_iter().find(|&h| opens(h))
}

pub fn hardware_failed() -> bool {
    HW_FAILED.load(Ordering::SeqCst)
}

/// The card was too slow lately and software decodes until the wait is over.
pub fn hardware_slow() -> bool {
    COOLDOWN.lock().unwrap().active()
}

/// Whether this Annex-B data holds an IDR picture, where a new decoder can start.
fn has_keyframe(data: &[u8]) -> bool {
    start_codes(data).into_iter().any(|(_, end)| data.get(end).is_some_and(|b| b & 31 == 5))
}

pub fn status() -> DecodingStatus {
    DecodingStatus {
        hardware_device: hardware_device().map(|h| h.name().to_string()),
        backends: LIVE.lock().unwrap().iter().filter(|(_, n)| **n > 0).map(|(k, _)| k.to_string()).collect(),
        hardware_failed: hardware_failed(),
        hardware_slow: hardware_slow(),
    }
}

fn backend(hw: Option<Hw>) -> &'static str {
    hw.map_or("processor", Hw::name)
}

fn count_live(hw: Option<Hw>, by: isize) {
    let mut live = LIVE.lock().unwrap();
    let n = live.entry(backend(hw)).or_insert(0);
    *n = n.saturating_add_signed(by);
}

/// Where each Annex-B start code is: (start, end) of the 00 00 01 or 00 00 00 01, left to right without overlap.
fn start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 2 < data.len() {
        if data[p] == 0 && data[p + 1] == 0 {
            if data[p + 2] == 1 {
                out.push((p, p + 3));
                p += 3;
                continue;
            }
            if data[p + 2] == 0 && p + 3 < data.len() && data[p + 3] == 1 {
                out.push((p, p + 4));
                p += 4;
                continue;
            }
        }
        p += 1;
    }
    out
}

/// Marks every plain Baseline SPS (profile 66) as Constrained Baseline, which FFmpeg's VA-API accepts and plain Baseline it refuses.
/// Phone encoders never use the Baseline-only tools (FMO, ASO, redundant slices), so the stream already is Constrained Baseline.
/// Only one bit of the byte after profile_idc changes: that can't make a 00 00 0x sequence, and the length stays the same.
fn mark_constrained_baseline(unit: &mut [u8]) {
    for (_, end) in start_codes(unit) {
        // NAL header, profile_idc, then the constraint flags
        if unit.len() > end + 2 && unit[end] & 31 == 7 && unit[end + 1] == 66 {
            unit[end + 2] |= 0x40;
        }
    }
}

/// An access unit delimiter, which ends the NAL unit before it and takes any stray bytes after it.
const END_OF_SETUP: &[u8] = b"\x00\x00\x00\x01\x09\xf0";

/// The stream's SPS and PPS as last seen, kept so a fresh decoder or a new packet step can start from a keyframe.
#[derive(Default)]
struct Headers {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    tail: Vec<u8>,
}

impl Headers {
    fn remember(&mut self, data: &[u8]) {
        let data = [&self.tail[..], data].concat();
        let starts = start_codes(&data);
        for pair in starts.windows(2) {
            let (first, last) = (pair[0], pair[1]);
            if first.1 < last.0 {
                match data[first.1] & 31 {
                    7 => self.sps = Some(data[first.0..last.0].to_vec()),
                    8 => self.pps = Some(data[first.0..last.0].to_vec()),
                    _ => {}
                }
            }
        }
        // Keep split start codes and incomplete SPS/PPS, without holding on to ordinary video frames
        self.tail = match starts.last() {
            Some(&(s, e)) if e < data.len() && matches!(data[e] & 31, 7 | 8) => {
                let rest = &data[s..];
                rest[rest.len().saturating_sub(65536)..].to_vec()
            }
            _ => data[data.len().saturating_sub(4)..].to_vec(),
        };
    }

    /// The SPS and PPS closed off by a delimiter, so data that starts mid-picture can't end up stuck to the PPS.
    fn setup(&self) -> Vec<u8> {
        [self.sps.as_deref().unwrap_or_default(), self.pps.as_deref().unwrap_or_default(), END_OF_SETUP].concat()
    }
}

/// What the timing probe reads: the recent pictures' times and the decoder they came from.
struct Timed {
    times: Timings,
    backend: &'static str,
}

pub struct H264 {
    codec: Codec,
    headers: Headers,
    tap: Option<Arc<Tap>>,
    // Steps of its own: ones that fed another decoder would hold this one's keyframes back
    steps: Option<Steps>,
    /// The steps' generation last fed, so a step put on later gets SPS and PPS first.
    step_generation: u64,
    /// Decoded a frame: from then on a bad packet is the stream's fault, not the card's.
    hw_proven: bool,
    /// Pieces of the stream the card has taken since its last picture.
    silent: u32,
    /// How long its recent pictures took (decode and, on the card, the copy to memory) and what it decodes on, shared with the timing probe.
    timings: Arc<Mutex<Timed>>,
    /// The frame rate the stream is meant to run at, which the stream changes while it runs.
    target_fps: Arc<AtomicU32>,
    /// Set while software decodes only because the card was too slow: the card to go back to once the wait is over.
    retry_hw: Option<Hw>,
    /// Tells the stream to ask for a keyframe after a change of decoder.
    ask_keyframe: Option<Box<dyn Fn() + Send>>,
    /// When the card gave up, until software has a picture.
    swapped_at: Option<std::time::Instant>,
    /// Tests: the next feed fails as a broken driver would.
    #[cfg(test)]
    break_next: bool,
}

impl H264 {
    /// On the card when hardware is asked for, one opens and none failed this run; software otherwise.
    pub fn new(hardware: bool, tap: Option<Arc<Tap>>) -> Result<H264, String> {
        H264::opening(hardware, hardware_device, Codec::h264, tap)
    }

    /// new, with how a computer finds its card decoder and opens a codec.
    fn opening(hardware: bool, device: impl FnOnce() -> Option<Hw>, open: impl Fn(Option<Hw>) -> Result<Codec, String>, tap: Option<Arc<Tap>>) -> Result<H264, String> {
        if hardware && hardware_failed() {
            log::info!("Decoding this stream in software: the graphics card failed earlier this run");
        }
        let mut wanted = if hardware && !hardware_failed() { device() } else { None };
        // The card was too slow lately: software until the wait is over, and then the card again
        let mut retry_hw = None;
        if let Some(h) = wanted {
            let now = Instant::now();
            let mut cooldown = COOLDOWN.lock().unwrap();
            if cooldown.waiting(now) {
                log::info!("Decoding this stream in software: the graphics card was too slow lately");
                retry_hw = wanted.take();
            } else if cooldown.active() {
                cooldown.returned(now);
                log::info!("Going back to hardware decoding ({}): the wait after it was too slow is over", h.name());
            }
        }
        let opened = wanted.map(|h| {
            let forced = matches!(Forced::asked(), Some((Forced::Open, _)));
            open(Some(h)).and_then(|c| if forced { Err("forced by TELESCOPE_HW_FAULT".to_string()) } else { Ok(c) }).map_err(|why| (h, why))
        });
        let codec = match opened {
            Some(Ok(c)) => c,
            Some(Err((h, why))) => {
                HW_FAILED.store(true, Ordering::SeqCst);
                log::warn!("Hardware decoding ({}) didn't start ({why}); decoding in software from now on", h.name());
                open(None)?
            }
            None => {
                if hardware && !hardware_failed() && retry_hw.is_none() {
                    log::info!("Decoding this stream in software: no graphics card decoder here");
                }
                open(None)?
            }
        };
        let mut dec = H264::with_codec(codec, tap);
        dec.retry_hw = retry_hw;
        Ok(dec)
    }

    fn with_codec(codec: Codec, tap: Option<Arc<Tap>>) -> H264 {
        let codec_hw = codec.hw;
        count_live(codec.hw, 1);
        let steps = tap.as_ref().map(|t| t.open());
        let step_generation = steps.as_ref().map_or(0, Steps::generation);
        H264 {
            codec,
            headers: Headers::default(),
            tap,
            steps,
            step_generation,
            hw_proven: false,
            silent: 0,
            timings: Arc::new(Mutex::new(Timed { times: Timings::default(), backend: backend(codec_hw) })),
            target_fps: Arc::new(AtomicU32::new(30)),
            retry_hw: None,
            ask_keyframe: None,
            swapped_at: None,
            #[cfg(test)]
            break_next: false,
        }
    }

    /// The card it decodes on, or None for software.
    pub fn hw(&self) -> Option<Hw> {
        self.codec.hw
    }

    /// The newest frame the data completed (older ones are dropped to keep latency down after a stall) and how many it completed.
    pub fn feed(&mut self, data: &[u8]) -> (Option<Frame>, u32) {
        self.headers.remember(data);
        let mut data = std::borrow::Cow::Borrowed(data);
        if let Some(steps) = &self.steps {
            let generation = steps.generation();
            if generation != self.step_generation {
                self.step_generation = generation;
                // TCP sends SPS and PPS only when joining, and a step put on later needs them
                if steps.on() {
                    data = [self.headers.setup(), data.into_owned()].concat().into();
                }
            }
        }
        // Joined the stream before its headers: nothing can decode that, so it says nothing about the card
        let strict = self.codec.hw.is_some() && !self.hw_proven && self.headers.sps.is_some() && self.headers.pps.is_some();
        let began = Instant::now();
        #[cfg(test)]
        let result = if std::mem::take(&mut self.break_next) {
            Err("the driver failed after receiving setup".to_string())
        } else {
            decode(&mut self.codec, self.steps.as_ref(), &data, strict)
        };
        #[cfg(not(test))]
        let result = decode(&mut self.codec, self.steps.as_ref(), &data, strict);
        match result {
            Ok(fed) if self.codec.hw.is_none() => {
                self.note_picture_back(fed.count);
                self.time(began, fed.count);
                if let Some(hw) = self.retry_hw.filter(|_| has_keyframe(&data)) {
                    if let Some(back) = self.go_back_to(hw, &data) {
                        return if back.0.is_some() { back } else { (fed.newest, fed.count) };
                    }
                }
                (fed.newest, fed.count)
            }
            Ok(fed) => {
                self.silent = if fed.count > 0 { 0 } else { self.silent + fed.units };
                self.hw_proven |= fed.count > 0;
                if self.silent >= SILENT_UNITS {
                    let why = format!("no picture from the last {} pieces of the stream", self.silent);
                    return self.fall_back(why, &data);
                }
                if let Some(why) = self.time(began, fed.count) {
                    // The card's picture is good, so it stays if software has none yet
                    return match self.fall_back_slow(why, &data) {
                        (None, _) => (fed.newest, fed.count),
                        swapped => swapped,
                    };
                }
                (fed.newest, fed.count)
            }
            Err(why) if self.codec.hw.is_none() => {
                log::warn!("H.264 decoding failed ({why}); skipping this data");
                telescope_core::stats::note_drop(telescope_core::stats::DropReason::DecoderFailed, 1);
                (None, 0)
            }
            Err(why) => self.fall_back(why, &data),
        }
    }

    /// Swaps the card for software mid-stream and retries data on it.
    fn fall_back(&mut self, why: String, data: &[u8]) -> (Option<Frame>, u32) {
        let hw = self.codec.hw.expect("only a card decoder falls back");
        HW_FAILED.store(true, Ordering::SeqCst);
        log::warn!("Hardware decoding ({}) failed ({why}); decoding in software from now on", hw.name());
        self.swap_to_software(hw, data)
    }

    /// Swaps the card for software because it's too slow, for a while: the card isn't called failed, and gets another go later.
    fn fall_back_slow(&mut self, why: String, data: &[u8]) -> (Option<Frame>, u32) {
        let hw = self.codec.hw.expect("only a card decoder falls back");
        let wait = COOLDOWN.lock().unwrap().fell(Instant::now());
        log::warn!("Hardware decoding ({}) is {why}; decoding in software for {} minutes, then trying the card again", hw.name(), wait.as_secs() / 60);
        let fed = self.swap_to_software(hw, data);
        if self.codec.hw.is_none() {
            self.retry_hw = Some(hw);
        }
        fed
    }

    /// Goes back to the card once the wait after it was too slow is over and software kept up meanwhile, at the next keyframe.
    fn go_back_to(&mut self, hw: Hw, data: &[u8]) -> Option<(Option<Frame>, u32)> {
        let now = Instant::now();
        {
            let mut cooldown = COOLDOWN.lock().unwrap();
            if cooldown.waiting(now) {
                return None;
            }
            if cooldown.active() {
                let interval = 1000.0 / f64::from(self.target_fps.load(Ordering::Relaxed).max(1));
                let timed = self.timings.lock().unwrap();
                if timed.times.slow(now, interval).is_some() || timed.times.stalled().is_some() {
                    log::info!("Staying in software a while longer: it's struggling with the stream too");
                    cooldown.extend(now);
                    return None;
                }
                cooldown.returned(now);
            }
        }
        self.retry_hw = None;
        let codec = match Codec::h264(Some(hw)) {
            Ok(c) => c,
            Err(why) => {
                HW_FAILED.store(true, Ordering::SeqCst);
                log::warn!("Hardware decoding ({}) didn't start again ({why}); decoding in software from now on", hw.name());
                return None;
            }
        };
        log::info!("Going back to hardware decoding ({}): software kept up while the card rested", hw.name());
        count_live(None, -1);
        count_live(Some(hw), 1);
        self.codec = codec;
        self.hw_proven = false;
        self.silent = 0;
        *self.timings.lock().unwrap() = Timed { times: Timings::default(), backend: backend(Some(hw)) };
        self.restart_steps();
        // A fresh decoder can't start from a keyframe alone, and TCP sends SPS and PPS only when joining
        let setup = [&self.headers.setup(), data].concat();
        Some(decode(&mut self.codec, self.steps.as_ref(), &setup, false).map_or((None, 0), |fed| (fed.newest, fed.count)))
    }

    /// Steps of its own for a new decoder: the old ones have already seen this data.
    fn restart_steps(&mut self) {
        if let Some(tap) = &self.tap {
            let steps = tap.open();
            self.step_generation = steps.generation();
            self.steps = Some(steps);
        }
    }

    fn swap_to_software(&mut self, hw: Hw, data: &[u8]) -> (Option<Frame>, u32) {
        let codec = match Codec::h264(None) {
            Ok(c) => c,
            Err(e) => {
                log::error!("The software decoder didn't open either: {e}");
                return (None, 0);
            }
        };
        count_live(Some(hw), -1);
        count_live(None, 1);
        self.codec = codec;
        *self.timings.lock().unwrap() = Timed { times: Timings::default(), backend: backend(None) };
        self.restart_steps();
        // Without this the picture waits for the phone's next keyframe, up to ten seconds with intra refresh
        if let Some(ask) = &self.ask_keyframe {
            ask();
        }
        // A fresh decoder can't start from a keyframe alone, and TCP sends SPS and PPS only when joining
        let setup = [&self.headers.setup(), data].concat();
        self.swapped_at = Some(std::time::Instant::now());
        let fed = decode(&mut self.codec, self.steps.as_ref(), &setup, false).map_or((None, 0), |fed| (fed.newest, fed.count));
        self.note_picture_back(fed.1);
        fed
    }

    /// Notes what each picture this data finished took, and says why the card is too slow for the stream if it has been for a whole window.
    fn time(&mut self, began: Instant, count: u32) -> Option<String> {
        if count == 0 {
            return None;
        }
        let now = Instant::now();
        let each = now.duration_since(began).as_secs_f64() * 1000.0 / f64::from(count);
        let fps = self.target_fps.load(Ordering::Relaxed).max(1);
        let interval = 1000.0 / f64::from(fps);
        let mut timed = self.timings.lock().unwrap();
        // The first picture of a decoder pays for its setup, which says nothing about the stream
        let first = timed.times.recent().is_none();
        for _ in 0..count.min(8) {
            timed.times.record(now, each);
        }
        if !first {
            timed.times.note_hiccup(now, each);
        }
        let hw = self.codec.hw?;
        if let Some(worst) = timed.times.stalled() {
            return Some(format!("stalled: pictures took up to {worst:.0} ms (decode and copy off the card), more than 500 ms once or 200 ms twice within {} s, on {}", slow::STALL_WINDOW.as_secs(), hw.name()));
        }
        let s = timed.times.slow(now, interval)?;
        Some(format!(
            "too slow for {} fps: a median of {:.1} ms, p95 {:.1} ms, max {:.1} ms a picture over the last {} s, against {interval:.1} ms between pictures on {}",
            fps, s.median_ms, s.p95_ms, s.max_ms, slow::WINDOW.as_secs(), hw.name()
        ))
    }

    fn note_picture_back(&mut self, count: u32) {
        if count > 0 {
            if let Some(at) = self.swapped_at.take() {
                log::info!("Software decoding has the picture back after {} ms", at.elapsed().as_millis());
            }
        }
    }
}

impl Drop for H264 {
    fn drop(&mut self) {
        count_live(self.codec.hw, -1);
    }
}

/// What feeding the decoder gave: the newest frame, how many finished and how many access units went in.
struct Fed {
    newest: Option<Frame>,
    count: u32,
    units: u32,
}

/// Parses data into access units, runs them through the packet steps and decodes them. strict: data the decoder
/// refuses is the decoder's fault (a card that can't take the stream), not something to skip.
fn decode(codec: &mut Codec, steps: Option<&Steps>, data: &[u8], strict: bool) -> Result<Fed, String> {
    let mut newest = None;
    let mut count = 0;
    let mut units_in = 0;
    let mut fault = None;
    let steps = steps.filter(|s| s.on());
    codec.parse(data, |codec, unit| {
        if fault.is_some() {
            return;
        }
        let units = match steps {
            Some(s) => s.step(unit),
            None => vec![unit.to_vec()],
        };
        for mut u in units {
            if codec.hw.is_some() {
                mark_constrained_baseline(&mut u);
            }
            units_in += 1;
            match codec.decode_newest(&u) {
                Ok(n) => count += n,
                // Joined mid-GOP, say: skip until a keyframe
                Err(Fault::Invalid) if !strict => {}
                Err(Fault::Invalid) => fault = Some("the decoder refused the stream".to_string()),
                Err(Fault::Broken(why)) => fault = Some(why),
            }
            if fault.is_some() {
                return;
            }
        }
    });
    // Only the newest picture is downloaded and converted; the ones it replaced never were
    if fault.is_none() {
        match codec.take_newest() {
            Ok(frame) => newest = frame,
            // A picture that couldn't be taken doesn't count as finished
            Err(Fault::Invalid) if !strict => count = count.saturating_sub(1),
            Err(Fault::Invalid) => fault = Some("the decoder refused the stream".to_string()),
            Err(Fault::Broken(why)) => fault = Some(why),
        }
    }
    codec.drop_newest();
    match fault {
        Some(why) => Err(why),
        None => Ok(Fed { newest, count, units: units_in }),
    }
}

impl Decoder for H264 {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        let (mut newest, count) = self.feed(&packet.data);
        if let Some(f) = newest.as_mut() {
            f.time_ns = packet.time_ns;
        }
        Decoded { newest, count }
    }

    fn hardware(&self) -> bool {
        self.codec.hw.is_some()
    }

    fn on_keyframe_needed(&mut self, ask: Box<dyn Fn() + Send>) {
        self.ask_keyframe = Some(ask);
    }

    fn target_fps_cell(&self) -> Option<Arc<AtomicU32>> {
        Some(self.target_fps.clone())
    }

    fn timing_probe(&self) -> Option<TimingProbe> {
        let timed = self.timings.clone();
        Some(Arc::new(move || {
            let timed = timed.lock().unwrap();
            let s = timed.times.recent()?;
            Some(format!("Decoding on {}: p95 {:.1} ms, max {:.1} ms a picture (decode and copy) over the last {} s", timed.backend, s.p95_ms, s.max_ms, slow::WINDOW.as_secs()))
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::packets::tests::{maker, FnStep};
    use crate::testenc;
    use std::sync::atomic::AtomicUsize;
    use telescope_core::media::PacketTap;

    pub(crate) const AUD: &[u8] = b"\x00\x00\x00\x01\x09\xf0";

    fn mean(f: &Frame) -> f64 {
        let y = &f.data[..(f.width * f.height) as usize];
        y.iter().map(|&v| v as f64).sum::<f64>() / y.len() as f64
    }

    /// The luma a flat grey of value v ends up as (BT.601 video range).
    fn luma(v: u8) -> f64 {
        16.0 + v as f64 * 219.0 / 255.0
    }

    fn software() -> H264 {
        H264::new(false, None).unwrap()
    }

    #[test]
    fn each_packet_shows_its_frame_straight_away() {
        let Some(packets) = testenc::h264(&[20, 60, 100, 140, 180, 220], 64, 48, 5) else { return };
        let mut dec = software();
        for (p, v) in packets.iter().zip([20, 60, 100, 140, 180, 220]) {
            let (frame, count) = dec.feed(p);
            let frame = frame.expect("a frame for every packet");
            // Frame i, not frame i - 1
            assert!(count == 1 && (mean(&frame) - luma(v)).abs() <= 8.0, "{} vs {}", mean(&frame), luma(v));
        }
    }

    #[test]
    fn pieces_of_any_size_decode_in_order() {
        let Some(packets) = testenc::h264(&[20, 60, 100, 140, 180, 220], 64, 48, 5) else { return };
        let data = packets.concat();
        let mut dec = software();
        let means: Vec<f64> = data.chunks(97).filter_map(|c| dec.feed(c).0).map(|f| mean(&f)).collect();
        assert!(means.windows(2).all(|w| w[0] <= w[1]) && *means.last().unwrap() > luma(200));
    }

    #[test]
    fn a_burst_shows_its_newest_frame_but_counts_every_frame_that_arrived() {
        let Some(packets) = testenc::h264(&[30, 60, 90, 120, 150, 180], 64, 48, 5) else { return };
        let (frame, count) = software().feed(&packets.concat());
        assert_eq!(count, 6);
        assert!((mean(&frame.unwrap()) - luma(180)).abs() <= 8.0);
    }

    #[test]
    fn decoding_stays_in_software_unless_hardware_is_on() {
        assert!(software().hw().is_none());
    }

    #[test]
    fn sps_and_pps_are_remembered_across_split_chunks_without_keeping_frames() {
        let Some(packets) = testenc::h264(&[50, 60], 64, 48, 5) else { return };
        let data = packets.concat();
        let mut h = Headers::default();
        for c in data.chunks(3) {
            h.remember(c);
        }
        let setup = h.setup();
        let kinds: Vec<u8> = start_codes(&setup).iter().map(|&(_, e)| setup[e] & 31).collect();
        assert_eq!(kinds, [7, 8, 9]);
        assert!(h.tail.len() <= 4);
    }

    #[test]
    fn a_baseline_sps_is_marked_constrained_baseline_and_nothing_else_changes() {
        let sps = [0, 0, 0, 1, 0x67, 66, 0x00, 0x1f, 0xe9, 0x40];
        let pps = [0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80];
        let mut unit = [&sps[..], &pps[..]].concat();
        let before = unit.clone();
        mark_constrained_baseline(&mut unit);
        assert_eq!(unit[6], 0x40);
        let diff: Vec<usize> = (0..unit.len()).filter(|&i| unit[i] != before[i]).collect();
        assert_eq!(diff, [6]);
        // Already marked, other profiles and other NAL units stay as they are
        let again = unit.clone();
        mark_constrained_baseline(&mut unit);
        assert_eq!(unit, again);
        let mut high = vec![0, 0, 1, 0x67, 100, 0, 40, 0xac];
        let copy = high.clone();
        mark_constrained_baseline(&mut high);
        assert_eq!(high, copy);
        let mut cut = vec![0, 0, 1, 0x67, 66];
        mark_constrained_baseline(&mut cut);
        assert_eq!(cut, [0, 0, 1, 0x67, 66]);
    }

    #[test]
    fn start_codes_match_three_and_four_byte_forms() {
        assert_eq!(start_codes(b"\x00\x00\x01\x67\x00\x00\x00\x01\x68"), [(0, 3), (4, 8)]);
        assert_eq!(start_codes(b"\x00\x00\x00\x00\x01\x65"), [(1, 5)]);
        assert!(start_codes(b"\x00\x00\x02\x00\x00").is_empty());
    }

    fn tap_with(make: Arc<dyn telescope_core::host::PacketStepFactory>) -> Arc<Tap> {
        let tap = Tap::new();
        tap.set(vec![("test".into(), make)]);
        tap
    }

    #[test]
    fn nothing_extra_runs_while_no_plugin_changes_packets() {
        let Some(packets) = testenc::h264(&[50; 8], 64, 48, 5) else { return };
        let mut dec = H264::new(false, Some(Tap::new())).unwrap();
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count == 8);
        assert!(!dec.steps.as_ref().unwrap().on());
        assert_eq!(dec.steps.as_ref().unwrap().asked(), 0, "no unit was copied out for a step");
    }

    #[test]
    fn a_step_sees_every_access_unit_and_what_it_hands_back_is_decoded() {
        let Some(packets) = testenc::h264(&[50; 6], 64, 48, 5) else { return };
        let seen = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let s = seen.clone();
        let tap = tap_with(maker(move || {
            let s = s.clone();
            Box::new(FnStep(move |u: &[u8]| {
                let mut seen = s.lock().unwrap();
                seen.push(u.to_vec());
                // The third twice
                if seen.len() == 3 { vec![u.to_vec(), u.to_vec()] } else { vec![u.to_vec()] }
            }))
        }));
        let (_, count) = H264::new(false, Some(tap)).unwrap().feed(&packets.concat());
        let seen = seen.lock().unwrap();
        assert!(seen.len() == 6 && seen.iter().all(|u| u.starts_with(b"\x00\x00")));
        assert_eq!(count, 7);
    }

    #[test]
    fn a_step_put_on_mid_stream_gets_the_streams_sps_and_pps_first() {
        // SPS and PPS only at the start, as TCP sends them
        let Some(packets) = testenc::h264(&[50; 12], 64, 48, 100) else { return };
        let tap = Tap::new();
        let mut dec = H264::new(false, Some(tap.clone())).unwrap();
        dec.feed(&packets[..6].concat());
        let units = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let u = units.clone();
        tap.set(vec![("test".into(), maker(move || {
            let u = u.clone();
            Box::new(FnStep(move |unit: &[u8]| {
                u.lock().unwrap().push(unit.to_vec());
                vec![unit.to_vec()]
            }))
        }))]);
        let (_, count) = dec.feed(&packets[6..].concat());
        let units = units.lock().unwrap();
        let first: Vec<u8> = start_codes(&units[0]).iter().map(|&(_, e)| units[0][e] & 31).collect();
        assert!(count > 0 && first.contains(&7) && first.contains(&8));
    }

    #[test]
    fn a_step_can_drop_a_frame() {
        let Some(packets) = testenc::h264(&[50; 10], 64, 48, 5) else { return };
        let n = Arc::new(AtomicUsize::new(0));
        let tap = tap_with(maker(move || {
            let n = n.clone();
            Box::new(FnStep(move |u: &[u8]| {
                // Drop every other non-keyframe picture
                let p_frame = start_codes(u).iter().any(|&(_, e)| u[e] & 31 == 1);
                if p_frame && n.fetch_add(1, Ordering::SeqCst) % 2 == 1 { vec![] } else { vec![u.to_vec()] }
            }))
        }));
        let (_, count) = H264::new(false, Some(tap)).unwrap().feed(&packets.concat());
        assert!(count < 10);
    }

    #[test]
    fn a_new_decoder_starts_its_steps_afresh() {
        let made = Arc::new(AtomicUsize::new(0));
        let m = made.clone();
        let tap = tap_with(maker(move || {
            m.fetch_add(1, Ordering::SeqCst);
            Box::new(FnStep(|u: &[u8]| vec![u.to_vec()]))
        }));
        let _a = H264::new(false, Some(tap.clone())).unwrap();
        let _b = H264::new(false, Some(tap)).unwrap();
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }

    /// HW_FAILED and the live count are one per run: tests that touch them take turns, and leave them as they found them.
    struct CardTest(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    fn card_test() -> CardTest {
        static TURN: Mutex<()> = Mutex::new(());
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        HW_FAILED.store(false, Ordering::SeqCst);
        CardTest(turn)
    }

    impl Drop for CardTest {
        fn drop(&mut self) {
            HW_FAILED.store(false, Ordering::SeqCst);
        }
    }

    fn hardware_in_use() -> bool {
        status().backends.iter().any(|b| b != "processor")
    }

    #[test]
    fn diagnostics_follow_the_live_decoders() {
        let dec = software();
        assert!(status().backends.contains(&"processor".to_string()));
        drop(dec);
    }

    #[test]
    fn diagnostics_follow_the_live_decoder_not_the_next_preference() {
        let _turn = card_test();
        let card = pretend_hardware(None);
        // Hardware decoding switched off: the next connection asks for software, and the running decoder doesn't hear of it
        let next = H264::new(false, None).unwrap();
        assert!(next.hw().is_none() && card.hw() == Some(Hw::Cuda));
        assert!(status().backends.contains(&"cuda".to_string()));
        assert!(hardware_in_use());
        drop(card);
        assert!(!hardware_in_use());
    }

    #[test]
    fn a_card_that_opens_decodes_or_hands_over_to_software() {
        // On a machine with VA-API this decodes on the card; anywhere else it's software, and either way frames come
        let Some(packets) = testenc::h264(&[60, 120, 180], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = H264::new(true, None).unwrap();
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count >= 1, "on {:?}", dec.hw());
    }

    /// A card decoder stands in for the test: software opened as if it were hardware, so the fallback logic runs.
    fn pretend_hardware(tap: Option<Arc<Tap>>) -> H264 {
        let mut codec = Codec::h264(None).unwrap();
        codec.hw = Some(Hw::Cuda);
        H264::with_codec(codec, tap)
    }

    #[test]
    fn a_graphics_card_that_fails_hands_over_to_software_and_stays_out() {
        let Some(packets) = testenc::h264(&[60, 120, 180], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.break_next = true;
        assert_eq!(dec.hw(), Some(Hw::Cuda));
        // The same input again in software
        let (frame, count) = dec.feed(&packets[0]);
        assert!(frame.is_some() && count == 1, "{count}");
        assert!(dec.hw().is_none() && hardware_failed() && status().hardware_failed);
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count == 3, "{count}");
        // Not tried again this run, even on a computer that has a card that opens
        let next = H264::opening(true, || Some(Hw::Cuda), |hw| {
            let mut codec = Codec::h264(None)?;
            codec.hw = hw;
            Ok(codec)
        }, None).unwrap();
        assert!(next.hw().is_none());
        assert!(!hardware_in_use());
    }

    #[test]
    fn no_graphics_card_decoder_means_software() {
        let Some(packets) = testenc::h264(&[90], 64, 48, 5) else { return };
        let _turn = card_test();
        let tried = Arc::new(AtomicUsize::new(0));
        let t = tried.clone();
        // The computer says it has one, and it doesn't open
        let mut dec = H264::opening(true, || Some(Hw::Cuda), move |hw| {
            if hw.is_some() {
                t.fetch_add(1, Ordering::SeqCst);
                return Err("no card here".into());
            }
            Codec::h264(None)
        }, None).unwrap();
        assert_eq!(tried.load(Ordering::SeqCst), 1);
        assert!(dec.hw().is_none() && hardware_failed(), "and it isn't tried again this run");
        assert!(dec.feed(&packets.concat()).0.is_some());
    }

    #[test]
    fn a_computer_without_a_card_decoder_isnt_a_card_that_failed() {
        let _turn = card_test();
        let dec = H264::opening(true, || None, Codec::h264, None).unwrap();
        assert!(dec.hw().is_none() && !hardware_failed());
    }

    /// Runs a real stream through a card decoder that goes wrong in the given way after two good frames.
    fn fails_mid_stream(kind: Forced) {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.codec.forced = Some((kind, 2));
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        dec.on_keyframe_needed(Box::new(move || {
            a.fetch_add(1, Ordering::SeqCst);
        }));
        let shown = packets.iter().filter(|p| dec.feed(p).0.is_some()).count();
        assert!(dec.hw().is_none() && hardware_failed(), "{kind:?} left it on the card");
        assert_eq!(asked.load(Ordering::SeqCst), 1, "{kind:?}");
        assert!(shown > 2, "{kind:?}: the picture came back after the swap, {shown} shown");
        assert!(status().backends.contains(&"processor".to_string()));
    }

    #[test]
    fn a_card_that_stops_giving_pictures_hands_over_mid_stream() {
        fails_mid_stream(Forced::Silent);
    }

    #[test]
    fn a_card_that_starts_refusing_every_packet_hands_over_mid_stream() {
        fails_mid_stream(Forced::Reject);
    }

    #[test]
    fn a_card_that_errors_mid_stream_hands_over() {
        fails_mid_stream(Forced::Broken);
    }

    #[test]
    fn a_moment_with_no_picture_shorter_than_the_limit_stays_on_the_card() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        for p in &packets[..2] {
            dec.feed(p);
        }
        dec.codec.forced = Some((Forced::Silent, 0));
        for p in &packets[2..2 + SILENT_UNITS as usize - 1] {
            dec.feed(p);
        }
        assert_eq!(dec.hw(), Some(Hw::Cuda));
        dec.codec.forced = None;
        dec.feed(&packets[2 + SILENT_UNITS as usize - 1]);
        assert_eq!(dec.silent, 0, "a picture clears the count");
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_failed());
    }

    #[test]
    fn joining_before_the_headers_says_nothing_about_the_card() {
        let Some(packets) = testenc::h264(&[60, 61], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        // A slice with no SPS or PPS seen yet: nothing could decode this
        dec.feed(b"\x00\x00\x00\x01\x41\x9a\x00\x00\x00\x01\x09\xf0");
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_failed());
        assert!(dec.feed(&packets.concat()).0.is_some());
    }

    /// Every way a real card goes wrong while copying its picture out: only VA-API can get here, so this skips elsewhere.
    #[test]
    fn a_real_card_that_gives_bad_pictures_hands_over() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 320, 240, 5) else { return };
        for kind in [Forced::Copy, Forced::Empty, Forced::Blank, Forced::Silent, Forced::Reject, Forced::Broken] {
            let _turn = card_test();
            let mut dec = H264::new(true, None).unwrap();
            if dec.hw().is_none() {
                return eprintln!("no graphics card decoder; skipped");
            }
            dec.codec.forced = Some((kind, 3));
            assert!(packets[..3].iter().all(|p| dec.feed(p).0.is_some()) && dec.hw().is_some(), "{kind:?}: good frames first, on the card");
            let shown = packets[3..].iter().filter(|p| dec.feed(p).0.is_some()).count();
            assert!(dec.hw().is_none() && hardware_failed(), "{kind:?} left it on the card");
            assert!(shown > 0, "{kind:?}: no picture after the swap");
        }
    }

    /// What a chunk of several access units gave before only the newest picture was extracted: every one converted, the last kept.
    fn newest_the_old_way(codec: &mut Codec, data: &[u8]) -> (Option<Frame>, u32) {
        let (mut newest, mut count) = (None, 0);
        codec.parse(data, |c, unit| count += c.decode(unit, |f| newest = Some(f)).unwrap());
        (newest, count)
    }

    #[test]
    fn a_burst_keeps_the_picture_extracting_each_one_would_have_kept() {
        let Some(packets) = testenc::h264_moving(40, 128, 96, 10, "high") else { return };
        let _turn = card_test();
        for hardware in [false, true] {
            for group in [1usize, 3, 4, 7] {
                let mut new = H264::new(hardware, None).unwrap();
                if hardware && new.hw().is_none() {
                    eprintln!("no graphics card decoder; skipped");
                    continue;
                }
                let mut old = Codec::h264(if hardware { new.hw() } else { None }).unwrap();
                let mut kept = 0;
                for chunk in packets.chunks(group) {
                    let data = chunk.concat();
                    let (got, count) = new.feed(&data);
                    let (want, want_count) = newest_the_old_way(&mut old, &data);
                    assert_eq!(count, want_count, "{group} units a chunk, hardware {hardware}");
                    assert_eq!(got.is_some(), want.is_some());
                    if let (Some(got), Some(want)) = (got, want) {
                        assert!(got.data == want.data, "{group} units a chunk, hardware {hardware}: a different picture was kept");
                        kept += 1;
                    }
                }
                assert!(kept > 5, "pictures came out");
            }
        }
    }

    #[test]
    fn a_real_card_gives_the_same_picture_as_software() {
        let Some(packets) = testenc::h264(&[40, 90, 140, 200], 320, 240, 5) else { return };
        let _turn = card_test();
        let mut card = H264::new(true, None).unwrap();
        if card.hw().is_none() {
            return eprintln!("no graphics card decoder; skipped");
        }
        let mut soft = H264::new(false, None).unwrap();
        for p in &packets {
            let (a, b) = (card.feed(p).0.unwrap(), soft.feed(p).0.unwrap());
            let worst = a.data.iter().zip(b.data.iter()).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
            assert!(worst <= 2, "off by {worst}");
        }
        assert!(card.hw().is_some(), "still on the card");
    }

    #[test]
    fn a_plain_baseline_stream_decodes_on_the_card() {
        let Some(mut packets) = testenc::h264_moving(8, 320, 240, 5, "baseline") else { return };
        let _turn = card_test();
        let mut card = H264::new(true, None).unwrap();
        if card.hw().is_none() {
            return eprintln!("no graphics card decoder; skipped");
        }
        // As the phone sends it: x264 marks its Baseline as constrained, the phone's encoder doesn't
        for p in &mut packets {
            for (_, end) in start_codes(&p.clone()) {
                if p[end] & 31 == 7 {
                    assert_eq!(p[end + 1], 66);
                    p[end + 2] &= !0x40;
                }
            }
        }
        let mut pictures = 0;
        for p in &packets {
            pictures += card.feed(&[&p[..], END_OF_SETUP].concat()).1;
        }
        assert!(pictures >= 1 && card.hw().is_some(), "{pictures} pictures, on {:?}", card.hw());
    }

    #[test]
    fn a_hardware_decoder_refusing_the_stream_falls_back_too() {
        let Some(packets) = testenc::h264(&[100], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.codec.refusals = u32::MAX;
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count == 1, "{count}");
        assert!(dec.hw().is_none() && hardware_failed());
    }

    #[test]
    fn garbage_a_card_refuses_before_its_first_frame_counts_as_the_card_failing() {
        // Real refusal from FFmpeg: a card decoder that hasn't decoded anything yet takes that as the card failing
        let Some(packets) = testenc::h264(&[60, 120], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        let bad = [b"\x00\x00\x00\x01\x65".as_slice(), &[0xFF; 64], AUD].concat();
        let (frame, count) = dec.feed(&[bad, packets.concat()].concat());
        assert!(dec.hw().is_none() && hardware_failed());
        assert!(frame.is_some() && count == 2);
    }

    #[test]
    fn the_software_fallback_gives_steps_each_unit_once() {
        let Some(packets) = testenc::h264(&[60, 120], 64, 48, 5) else { return };
        let _turn = card_test();
        let seen = Arc::new(Mutex::new(Vec::<usize>::new()));
        let s = seen.clone();
        let tap = tap_with(maker(move || {
            let s = s.clone();
            let index = { let mut v = s.lock().unwrap(); v.push(0); v.len() - 1 };
            Box::new(FnStep(move |u: &[u8]| {
                s.lock().unwrap()[index] += 1;
                vec![u.to_vec()]
            }))
        }));
        let mut dec = pretend_hardware(Some(tap));
        dec.codec.refusals = u32::MAX;
        assert_eq!(dec.hw(), Some(Hw::Cuda));
        let (frame, _) = dec.feed(&packets.concat());
        assert!(frame.is_some() && dec.hw().is_none());
        // A fresh step for the software decoder, given each unit once
        let seen = seen.lock().unwrap();
        assert_eq!((seen.len(), seen[1]), (2, 2), "{seen:?}");
    }

    #[test]
    fn a_bad_packet_after_hardware_decoded_frames_is_skipped_not_a_fallback() {
        let Some(packets) = testenc::h264(&[60, 61, 62, 63, 64], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        assert!(dec.feed(&packets[0]).0.is_some());
        dec.codec.refusals = 1;
        dec.feed(&packets[1]);
        assert_eq!(dec.codec.refusals, 0, "the packet reached the decoder");
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_failed());
        assert!(dec.feed(&packets[2..].concat()).0.is_some());
    }

    #[test]
    fn failure_after_setup_recovers_at_an_idr_without_repeated_headers() {
        let Some(packets) = testenc::h264(&(60..75).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        assert!(dec.feed(&packets[0]).0.is_some());
        dec.break_next = true;
        // Every later packet without its SPS and PPS, as TCP sends them after joining
        let recovered: Vec<bool> = packets[1..].iter().map(|p| {
            let codes = start_codes(p);
            let mut stripped = Vec::new();
            for (i, &(s, e)) in codes.iter().enumerate() {
                let end = codes.get(i + 1).map_or(p.len(), |c| c.0);
                if e < end && !matches!(p[e] & 31, 7 | 8) {
                    stripped.extend_from_slice(&p[s..end]);
                }
            }
            dec.feed(&stripped).0.is_some()
        }).collect();
        assert!(recovered.iter().any(|&r| r) && dec.hw().is_none());
    }

    /// The NAL units of an Annex-B packet, each with its start code.
    fn nal_units(p: &[u8]) -> Vec<&[u8]> {
        let codes = start_codes(p);
        codes.iter().enumerate().map(|(i, &(s, _))| &p[s..codes.get(i + 1).map_or(p.len(), |c| c.0)]).collect()
    }

    #[test]
    fn software_taking_over_is_given_a_pps_with_nothing_stuck_to_it() {
        // The card refuses a keyframe that arrived in pieces, so software gets its tail after the setup: that tail must not stick to the PPS
        let Some(packets) = testenc::h264_noisy(&[60, 120, 180], 128, 96, 5) else { return };
        let _turn = card_test();
        let seen = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let s = seen.clone();
        let tap = tap_with(maker(move || {
            let s = s.clone();
            Box::new(FnStep(move |u: &[u8]| {
                s.lock().unwrap().push(u.to_vec());
                vec![u.to_vec()]
            }))
        }));
        let mut dec = pretend_hardware(Some(tap));
        dec.codec.refusals = u32::MAX;
        let units = nal_units(&packets[0]);
        let pps_len = units.iter().find(|u| u[start_codes(u)[0].1] & 31 == 8).unwrap().len();
        let (setup, idr): (Vec<&[u8]>, Vec<&[u8]>) = units.into_iter().partition(|u| matches!(u[start_codes(u)[0].1] & 31, 7 | 8));
        let (setup, idr) = (setup.concat(), idr.concat());
        dec.feed(&setup);
        dec.feed(&idr[..idr.len() / 2]);
        dec.feed(&idr[idr.len() / 2..]);
        assert!(dec.hw().is_none());
        dec.feed(&packets[1..].concat());
        for unit in seen.lock().unwrap().iter() {
            for nal in nal_units(unit).into_iter().filter(|n| n[start_codes(n)[0].1] & 31 == 8) {
                assert_eq!(nal.len(), pps_len, "the PPS has {} bytes after it", nal.len() as isize - pps_len as isize);
            }
        }
    }

    /// A card that took ms a picture for the last few seconds at 30 fps, as the decoder would have timed it.
    fn card_that_took(dec: &H264, ms: f64) {
        let now = Instant::now();
        let mut times = slow::Timings::default();
        for i in 0..100u32 {
            times.record(now - std::time::Duration::from_millis(3400 - u64::from(i) * 33), ms);
        }
        dec.timings.lock().unwrap().times = times;
    }

    #[test]
    fn a_card_too_slow_for_the_stream_hands_over_to_software_for_a_while() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        *COOLDOWN.lock().unwrap() = slow::Cooldown::new();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        // A slow integrated card: 45 ms a picture against 33 ms between pictures
        card_that_took(&dec, 45.0);
        assert!(dec.feed(&packets[1]).0.is_some());
        let switched = dec.hw().is_none() && !hardware_failed() && hardware_slow() && dec.retry_hw == Some(Hw::Cuda);
        *COOLDOWN.lock().unwrap() = slow::Cooldown::new();
        assert!(switched, "still on {:?}", dec.hw());
    }

    #[test]
    fn a_card_that_keeps_up_stays_on_the_card() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        *COOLDOWN.lock().unwrap() = slow::Cooldown::new();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        card_that_took(&dec, 6.0);
        assert!(dec.feed(&packets[1]).0.is_some());
        let stayed = dec.hw() == Some(Hw::Cuda) && !hardware_slow();
        *COOLDOWN.lock().unwrap() = slow::Cooldown::new();
        assert!(stayed, "left the card for {:?}", dec.hw());
    }

    #[test]
    fn linux_tries_va_api_before_cuda_when_ffmpeg_has_it() {
        #[cfg(not(windows))]
        {
            assert_eq!(first_that_opens(|_| true), Some(Hw::Vaapi));
            // FFmpeg built without VA-API, or a card without it
            assert_eq!(first_that_opens(|h| h == Hw::Cuda), Some(Hw::Cuda));
        }
        #[cfg(windows)]
        {
            assert_eq!(first_that_opens(|_| true), Some(Hw::D3d11va));
            assert_eq!(first_that_opens(|h| h == Hw::Cuda), Some(Hw::Cuda));
        }
        assert_eq!(first_that_opens(|_| false), None);
    }

    #[test]
    fn a_hand_over_decoder_doesnt_take_the_running_decoders_steps() {
        // A step that lets only the first keyframe through, like a datamosh: one shared between decoders would give
        // the new route's decoder nothing to start from
        let Some(packets) = testenc::h264(&(40..120).step_by(5).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let tap = tap_with(maker(|| {
            let mut seen_key = false;
            Box::new(FnStep(move |u: &[u8]| {
                let key = start_codes(u).iter().any(|&(_, e)| u[e] & 31 == 5);
                if key && std::mem::replace(&mut seen_key, true) { vec![] } else { vec![u.to_vec()] }
            }))
        }));
        let stream = packets.concat();
        let mut running = H264::new(false, Some(tap.clone())).unwrap();
        assert!(running.feed(&stream).1 > 0);
        let mut new = H264::new(false, Some(tap)).unwrap();
        running.feed(&stream);
        assert!(new.feed(&stream).1 > 0);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::testenc;
    use std::time::Instant;

    /// cargo test -p telescope-media --release -- --ignored --nocapture decode_speed
    #[test]
    #[ignore]
    fn decode_speed() {
        let Some(packets) = testenc::h264(&(0..120).map(|i| (i * 2) as u8).collect::<Vec<_>>(), 1920, 1080, 30) else { return };
        for hardware in [false, true] {
            let mut dec = H264::new(hardware, None).unwrap();
            let start = Instant::now();
            let mut last = None;
            let frames: u32 = packets.iter().map(|p| {
                let (f, n) = dec.feed(p);
                last = f.or(last.take());
                n
            }).sum();
            let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
            // The same picture either way: luma and chroma of the last frame
            let f = last.unwrap();
            let (y, u, v) = f.i420_planes().unwrap();
            let avg = |p: &[u8]| p.iter().map(|&b| b as u64).sum::<u64>() / p.len() as u64;
            println!("{:?}: {frames} frames at 1080p, {ms:.2} ms each, hardware failed: {}, last frame Y/U/V {}/{}/{}",
                     dec.hw(), hardware_failed(), avg(y), avg(u), avg(v));
        }
    }

    /// Process CPU time in seconds (user and system, every thread) from /proc.
    fn cpu_seconds() -> f64 {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let fields: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(11) + ticks(12)) / 100.0
    }

    /// Decode time and CPU of a whole clip a frame at a time, software against the card.
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture card_against_software
    #[test]
    #[ignore = "a measurement, needs the clips and a VA-API card"]
    fn card_against_software() {
        use crate::ffmpeg::decode_bench::packets;
        for name in ["p1080.h264", "p4k.h264"] {
            let Some(packets) = packets(name) else { continue };
            for hardware in [false, true, false, true] {
                let mut dec = H264::new(hardware, None).unwrap();
                let (cpu, began) = (cpu_seconds(), Instant::now());
                let frames: u32 = packets.iter().map(|p| dec.feed(p).1).sum();
                let (wall, cpu) = (began.elapsed().as_secs_f64(), cpu_seconds() - cpu);
                println!("{name:11} {:>9?}: {frames} pictures, {:.2} ms each, {:.2} ms of cpu each, hardware failed: {}",
                         dec.hw(), wall * 1e3 / frames as f64, cpu * 1e3 / frames as f64, hardware_failed());
            }
        }
    }

    /// A stream fed in chunks of several access units, as after a stall: only the newest picture of each chunk is kept.
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture burst_feeding
    #[test]
    #[ignore = "a measurement, needs the clips"]
    fn burst_feeding() {
        use crate::ffmpeg::decode_bench::{digest, packets};
        for hardware in [false, true] {
            for name in ["p1080.h264", "p4k.h264"] {
                let Some(packets) = packets(name) else { continue };
                for group in [1usize, 2, 4, 8] {
                    let mut best = f64::MAX;
                    let (mut hash, mut backend, mut finished) = (0, None, 0);
                    for _ in 0..3 {
                        let mut dec = H264::new(hardware, None).unwrap();
                        let (mut newest, mut times) = (Vec::new(), Vec::new());
                        finished = 0;
                        for chunk in packets.chunks(group) {
                            let data = chunk.concat();
                            let began = Instant::now();
                            let (f, n) = dec.feed(&data);
                            times.push(began.elapsed().as_secs_f64() * 1e3);
                            finished += n;
                            newest.extend(f);
                        }
                        assert!(newest.len() > 10, "no pictures kept");
                        let total: f64 = times.iter().sum();
                        if total < best {
                            best = total;
                            hash = digest(&newest);
                            backend = dec.hw();
                        }
                    }
                    println!("{:>7?} {name:11} {group} access units per feed: {:6.2} ms per picture, {finished} pictures, kept hash {hash:016x}", backend, best / finished.max(1) as f64);
                }
            }
        }
    }
}
