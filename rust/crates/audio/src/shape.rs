//! What the writer does to each chunk (AudioWorker._shape, _mute and _measure): noise reduction, gain, automatic
//! level, the limiter, clipping to 16 bits and muting, plus the level the card's meter shows.

use crate::denoise::{self, Denoise};
use crate::jitter::{linspace, rms, take_samples, CHUNK, FULL_SCALE};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// The limiter keeps peaks under -1 dBFS.
pub fn limit_ceiling() -> f32 {
    10f32.powf(-1.0 / 20.0)
}
/// Per chunk once the peak has passed: 25 dB/s.
fn limit_release() -> f32 {
    10f32.powf(0.25 / 20.0)
}
// Automatic level: speech brought toward AUTO_TARGET_DB (RMS), within these bounds, and only while someone speaks
pub const AUTO_TARGET_DB: f64 = -20.0;
pub const AUTO_MAX_BOOST_DB: f64 = 18.0;
pub const AUTO_MAX_CUT_DB: f64 = -12.0;
/// Over the room's own noise floor counts as speech; the gain holds during pauses...
pub const AUTO_GATE_DB: f64 = 10.0;
/// ...and so does anything this far under how loud speech has been (a fan starting up).
pub const AUTO_GATE_BELOW_SPEECH_DB: f64 = 18.0;
/// Quieter than this is never speech.
pub const AUTO_GATE_FLOOR_DB: f64 = -60.0;
/// The noise floor is the quietest 100 ms over this long: speech pauses at least that often.
pub const AUTO_FLOOR_S: f64 = 2.0;
/// Per 10 ms chunk: 6 dB/s up, slow so it doesn't pump...
pub const AUTO_UP_DB: f64 = 0.06;
/// ...and 30 dB/s down, so a loud bit comes down quickly (the limiter catches the first of it).
pub const AUTO_DOWN_DB: f64 = 0.3;
/// Per chunk: the speech level it goes by is an average over about half a second.
pub const AUTO_LEVEL_RATE: f64 = 0.02;
/// Samples (1 ms) to reach a new, lower gain; hard clipping catches anything in between.
pub const LIMIT_ATTACK: usize = 48;
/// Chunks (2 s) noise reduction waits for a pause to switch in, before switching anyway.
pub const NR_SWITCH_WAIT: u32 = 200;

fn db_to_gain(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// The settings the card changes while the worker runs, read by its threads.
pub struct Controls {
    gain: AtomicU32,
    muted: AtomicBool,
    limit: AtomicBool,
    auto_level: AtomicBool,
    noise_reduction: AtomicBool,
    /// Automatic level's gain now, from the writer, for the level the reader measures.
    auto_db: AtomicU32,
    level: Mutex<(f32, f32, bool)>,
}

impl Default for Controls {
    fn default() -> Self {
        Controls {
            gain: AtomicU32::new(1f32.to_bits()),
            muted: AtomicBool::new(false),
            limit: AtomicBool::new(true),
            auto_level: AtomicBool::new(false),
            noise_reduction: AtomicBool::new(false),
            auto_db: AtomicU32::new(0f32.to_bits()),
            level: Mutex::new((0.0, 0.0, false)),
        }
    }
}

impl Controls {
    /// Linear; changes fade in over a chunk like mute does.
    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
    /// The output gets silence, so apps keep the mic but hear nothing.
    pub fn set_muted(&self, on: bool) {
        self.muted.store(on, Ordering::Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    /// Squash peaks smoothly instead of letting gain clip them.
    pub fn set_limit(&self, on: bool) {
        self.limit.store(on, Ordering::Relaxed);
    }
    pub fn limit(&self) -> bool {
        self.limit.load(Ordering::Relaxed)
    }
    /// Bring speech to a steady level, after gain and before the limiter.
    pub fn set_auto_level(&self, on: bool) {
        self.auto_level.store(on, Ordering::Relaxed);
    }
    pub fn auto_level(&self) -> bool {
        self.auto_level.load(Ordering::Relaxed)
    }
    /// RNNoise, before everything else.
    pub fn set_noise_reduction(&self, on: bool) {
        self.noise_reduction.store(on, Ordering::Relaxed);
    }
    pub fn noise_reduction(&self) -> bool {
        self.noise_reduction.load(Ordering::Relaxed)
    }
    fn auto_db(&self) -> f32 {
        f32::from_bits(self.auto_db.load(Ordering::Relaxed))
    }

    /// (peak, rms, limited) since the last call: fractions of full scale after gain and limiter, before muting.
    pub fn take_level(&self) -> (f32, f32, bool) {
        std::mem::replace(&mut *self.level.lock().unwrap_or_else(|e| e.into_inner()), (0.0, 0.0, false))
    }
}

/// Measures audio as it arrives, not as it's written: PipeWire stops reading an idle mic, which stalls the writer.
#[derive(Default)]
pub struct Measure {
    /// A reply can end mid-sample; its first byte waits for the next one.
    odd: Vec<u8>,
}

impl Measure {
    pub fn reset(&mut self) {
        self.odd.clear();
    }

    pub fn measure(&mut self, controls: &Controls, data: &[u8]) {
        let samples = take_samples(&mut self.odd, data);
        if samples.is_empty() {
            return;
        }
        let auto = if controls.auto_level() { db_to_gain(controls.auto_db() as f64) } else { 1.0 };
        let gain = controls.gain() * auto / FULL_SCALE;
        let peak = samples.iter().fold(0f32, |m, v| m.max(v.abs())) * gain;
        let level = rms(&samples) as f32 * gain;
        let ceiling = limit_ceiling();
        let limited = controls.limit() && peak > ceiling;
        let top = if controls.limit() { ceiling } else { 1.0 }; // what the output can reach
        let mut l = controls.level.lock().unwrap_or_else(|e| e.into_inner());
        l.0 = l.0.max(peak.min(top));
        l.1 = l.1.max(level.min(top));
        l.2 = l.2 || limited;
    }
}

pub type MakeDenoiser = Box<dyn FnMut() -> Result<Option<Box<dyn Denoise>>, String> + Send>;

/// The writer's own state between chunks.
pub struct Shaper {
    make_denoiser: Option<MakeDenoiser>,
    /// Made when switched on, and only ever used here.
    denoiser: Option<Box<dyn Denoise>>,
    /// 1.0 once the cleaned audio is what's written; switching fades over a chunk.
    pub(crate) wet: f32,
    /// Chunks a switch of noise_reduction has waited for a pause.
    nr_waited: u32,
    /// The last chunks, to start RNNoise on.
    recent: VecDeque<Vec<i16>>,
    pub(crate) auto_db: f64,
    /// How loud speech has been lately (RMS, dBFS); None before any.
    speech_db: Option<f64>,
    /// Per 100 ms, the quietest chunk.
    quietest: VecDeque<f64>,
    chunks_in_block: u32,
    /// The limiter's current gain reduction.
    pub(crate) limit_gain: f32,
    /// Where the last chunk's gain fade ended.
    gain: f32,
    /// Where the last chunk's mute fade ended.
    mute_gain: f32,
}

impl Default for Shaper {
    fn default() -> Self {
        Shaper::new(Box::new(denoise::make))
    }
}

impl Shaper {
    pub fn new(make_denoiser: MakeDenoiser) -> Shaper {
        Shaper {
            make_denoiser: Some(make_denoiser),
            denoiser: None,
            wet: 0.0,
            nr_waited: 0,
            recent: VecDeque::new(),
            auto_db: 0.0,
            speech_db: None,
            quietest: VecDeque::new(),
            chunks_in_block: 0,
            limit_gain: 1.0,
            gain: 1.0,
            mute_gain: 1.0,
        }
    }

    fn remember(&mut self, data: &[i16]) {
        if self.recent.len() == denoise::DELAY / CHUNK {
            self.recent.pop_front();
        }
        self.recent.push_back(data.to_vec());
    }

    /// Noise reduction, gain, automatic level, then the limiter, then clipping to 16 bits. quiet_limit is the
    /// jitter buffer's: the RMS a pause stays under.
    pub fn shape(&mut self, c: &Controls, data: &[i16], observe_level: bool, quiet_limit: f64) -> Vec<i16> {
        // RNNoise goes first: it's trained on a mic as it comes, and Auto level then judges speech against a quiet room
        let cleaned = self.reduce_noise(c, data, quiet_limit);
        let target = c.gain();
        let ceiling = limit_ceiling();
        if cleaned.is_none() && !c.auto_level() && self.auto_db == 0.0 && target == 1.0 && self.gain == 1.0
            && (!c.limit() || (self.limit_gain == 1.0 && data.iter().map(|&v| (v as i32).abs()).max().unwrap_or(0) as f32 <= ceiling * FULL_SCALE))
        {
            self.limit_gain = 1.0;
            return data.to_vec(); // nothing to do, the usual case
        }
        let mut s: Vec<f32> = cleaned.unwrap_or_else(|| data.iter().map(|&v| v as f32).collect());
        if target != self.gain {
            for (v, g) in s.iter_mut().zip(linspace(self.gain as f64, target as f64, data.len())) {
                *v *= g; // a jump in gain clicks too
            }
            self.gain = target;
        } else if target != 1.0 {
            s.iter_mut().for_each(|v| *v *= target);
        }
        if c.auto_level() {
            if observe_level {
                s = self.auto(s);
            } else {
                let g = db_to_gain(self.auto_db);
                s.iter_mut().for_each(|v| *v *= g);
            }
        } else if self.auto_db != 0.0 {
            // Switched off: its gain fades out over this chunk, and it starts over if switched on again
            for (v, g) in s.iter_mut().zip(linspace(10f64.powf(self.auto_db / 20.0), 1.0, data.len())) {
                *v *= g;
            }
            self.auto_db = 0.0;
            self.speech_db = None;
            self.quietest.clear();
        }
        c.auto_db.store((self.auto_db as f32).to_bits(), Ordering::Relaxed);
        if c.limit() {
            s = self.limit(s);
        } else {
            self.limit_gain = 1.0;
        }
        s.iter().map(|&v| v.clamp(-32768.0, FULL_SCALE) as i16).collect()
    }

    /// The chunk through RNNoise once it's switched in; None while the plain audio is written.
    fn reduce_noise(&mut self, c: &Controls, data: &[i16], quiet_limit: f64) -> Option<Vec<f32>> {
        let want = if c.noise_reduction() { 1.0 } else { 0.0 };
        if want == 0.0 && self.wet == 0.0 {
            self.remember(data);
            self.drop_denoiser();
            return None;
        }
        let dry: Vec<f32> = data.iter().map(|&v| v as f32).collect();
        if self.denoiser.is_none() {
            let made = match self.make_denoiser.as_mut().map(|make| make()) {
                Some(Ok(d)) => d,
                Some(Err(e)) => {
                    log::error!("Couldn't start noise reduction: {e}");
                    self.make_denoiser = None; // logged once; the audio carries on without it
                    None
                }
                None => None,
            };
            let Some(mut d) = made else {
                self.remember(data);
                return None;
            };
            for old in &self.recent {
                // so it has its lookahead already and doesn't open with 20 ms of silence
                d.process(&old.iter().map(|&v| v as f32).collect::<Vec<_>>());
            }
            self.denoiser = Some(d);
        }
        let wet = self.denoiser.as_mut().map(|d| d.process(&dry)).unwrap_or_default();
        let earlier: Vec<Vec<i16>> = self.recent.iter().cloned().collect();
        self.remember(data);
        if want == self.wet {
            self.nr_waited = 0;
            return Some(wet);
        }
        // RNNoise's output is 20 ms behind: switching repeats or skips 20 ms, so it waits for a pause that long
        self.nr_waited += 1;
        let quiet = |chunk: &[i16]| rms(&chunk.iter().map(|&v| v as f32).collect::<Vec<_>>()) < quiet_limit;
        if self.nr_waited < NR_SWITCH_WAIT && !(earlier.iter().all(|c| quiet(c)) && quiet(data)) {
            return if self.wet != 0.0 { Some(wet) } else { None };
        }
        self.nr_waited = 0;
        let ramp = linspace(self.wet as f64, want as f64, dry.len());
        self.wet = want;
        Some(dry.iter().zip(&wet).zip(ramp).map(|((d, w), r)| d + (w - d) * r).collect())
    }

    /// The writer stopped (or noise reduction is off): lets go of RNNoise.
    pub fn drop_denoiser(&mut self) {
        self.denoiser = None;
        self.wet = 0.0;
        self.nr_waited = 0;
    }

    pub fn has_denoiser(&self) -> bool {
        self.denoiser.is_some()
    }

    /// Automatic level: follows how loud speech is and moves its gain toward the target, holding it in pauses.
    fn auto(&mut self, mut s: Vec<f32>) -> Vec<f32> {
        if s.is_empty() {
            return s;
        }
        let level = rms(&s) / FULL_SCALE as f64;
        let db = 20.0 * level.max(1e-9).log10();
        if db < -90.0 {
            // Digital silence (startup, a muted source, a recording gap) says nothing about the room's noise floor.
            let g = db_to_gain(self.auto_db);
            s.iter_mut().for_each(|v| *v *= g);
            return s;
        }
        if self.chunks_in_block == 0 || self.quietest.is_empty() {
            if self.quietest.len() == (AUTO_FLOOR_S * 10.0) as usize {
                self.quietest.pop_front();
            }
            self.quietest.push_back(db);
        } else if let Some(last) = self.quietest.back_mut() {
            *last = last.min(db);
        }
        self.chunks_in_block = (self.chunks_in_block + 1) % 10;
        let floor = self.quietest.iter().copied().fold(f64::INFINITY, f64::min);
        let mut gate = (floor + AUTO_GATE_DB).max(AUTO_GATE_FLOOR_DB);
        if let Some(speech) = self.speech_db {
            gate = gate.max(speech - AUTO_GATE_BELOW_SPEECH_DB);
        }
        let before = self.auto_db;
        if db > gate {
            let speech = match self.speech_db {
                None => db,
                Some(s) => s + (db - s) * AUTO_LEVEL_RATE,
            };
            self.speech_db = Some(speech);
            let want = (AUTO_TARGET_DB - speech).clamp(AUTO_MAX_CUT_DB, AUTO_MAX_BOOST_DB);
            self.auto_db += (want - before).clamp(-AUTO_DOWN_DB, AUTO_UP_DB);
        }
        if before == 0.0 && self.auto_db == 0.0 {
            return s;
        }
        let n = s.len();
        for (v, g) in s.iter_mut().zip(linspace(10f64.powf(before / 20.0), 10f64.powf(self.auto_db / 20.0), n)) {
            *v *= g; // a jump in gain clicks
        }
        s
    }

    fn limit(&mut self, mut s: Vec<f32>) -> Vec<f32> {
        let ceiling = limit_ceiling();
        let peak = s.iter().fold(0f32, |m, v| m.max(v.abs())) / FULL_SCALE;
        let need = if peak > ceiling { ceiling / peak } else { 1.0 };
        let target = need.min(self.limit_gain * limit_release()).min(1.0);
        if target == 1.0 && self.limit_gain == 1.0 {
            return s;
        }
        let ramp = if target < self.limit_gain {
            let mut r = vec![target; s.len()]; // down fast, so the peak is already under
            let n = LIMIT_ATTACK.min(s.len());
            r[..n].copy_from_slice(&linspace(self.limit_gain as f64, target as f64, n));
            r
        } else {
            linspace(self.limit_gain as f64, target as f64, s.len()) // back up slowly
        };
        self.limit_gain = target;
        s.iter_mut().zip(ramp).for_each(|(v, g)| *v *= g);
        s
    }

    /// Fades over one chunk (10 ms): cutting mid-wave clicks.
    pub fn mute(&mut self, c: &Controls, data: Vec<i16>) -> Vec<i16> {
        let target = if c.muted() { 0.0 } else { 1.0 };
        if target == self.mute_gain {
            return if target != 0.0 { data } else { vec![0; data.len()] };
        }
        let ramp = linspace(self.mute_gain as f64, target as f64, data.len());
        self.mute_gain = target;
        data.iter().zip(ramp).map(|(&v, g)| (v as f32 * g) as i16).collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::jitter::{tests::biggest_step, JitterBuffer, RATE};
    use std::f64::consts::PI;
    use std::sync::{Arc, Mutex};

    pub fn quiet_limit() -> f64 {
        JitterBuffer::default().quiet_limit()
    }

    pub fn chunk(v: i16) -> Vec<i16> {
        vec![v; CHUNK]
    }

    pub fn bytes(x: &[i16]) -> Vec<u8> {
        x.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    pub fn ints(x: &[f64]) -> Vec<i16> {
        x.iter().map(|&s| s.round_ties_even().clamp(-32768.0, 32767.0) as i16).collect()
    }

    struct W {
        c: Controls,
        s: Shaper,
    }

    impl W {
        fn new() -> W {
            W::with(Box::new(|| Ok(None)))
        }
        fn with(make: MakeDenoiser) -> W {
            W { c: Controls::default(), s: Shaper::new(make) }
        }
        fn shape(&mut self, x: &[i16]) -> Vec<i16> {
            self.s.shape(&self.c, x, true, quiet_limit())
        }
        fn shape_all(&mut self, x: &[f64]) -> Vec<f32> {
            let x = ints(x);
            x.as_chunks::<CHUNK>().0.iter().flat_map(|c| self.shape(c)).map(|v| v as f32).collect()
        }
    }

    #[test]
    fn muting_fades_out_over_one_chunk_then_writes_silence() {
        let mut w = W::new();
        assert_eq!(w.s.mute(&w.c, chunk(1000)), chunk(1000));
        w.c.set_muted(true);
        let faded = w.s.mute(&w.c, chunk(1000));
        assert!(faded[0] == 1000 && *faded.last().unwrap() == 0 && faded.windows(2).all(|p| p[1] <= p[0]));
        assert_eq!(w.s.mute(&w.c, chunk(1000)), chunk(0));
        w.c.set_muted(false);
        let back = w.s.mute(&w.c, chunk(1000));
        assert!(back[0] == 0 && *back.last().unwrap() == 1000);
        assert_eq!(w.s.mute(&w.c, chunk(1000)), chunk(1000));
    }

    #[test]
    fn level_is_the_loudest_since_the_last_look_and_ignores_mute() {
        let c = Controls::default();
        let mut m = Measure::default();
        c.set_muted(true);
        m.measure(&c, &bytes(&chunk(16384)));
        m.measure(&c, &bytes(&chunk(-32768)));
        m.measure(&c, &bytes(&chunk(100)));
        let (peak, _, limited) = c.take_level();
        assert!((peak - limit_ceiling()).abs() < 1e-6 && limited); // the limiter's ceiling, not a clip
        c.set_limit(false);
        m.measure(&c, &bytes(&chunk(-32768)));
        let (peak, level, _) = c.take_level();
        assert!(peak == 1.0 && (level - 1.0).abs() < 0.001);
        assert_eq!(c.take_level(), (0.0, 0.0, false));
    }

    #[test]
    fn gain_fades_to_its_new_level_and_clips_at_full_scale_without_the_limiter() {
        let mut w = W::new();
        w.c.set_limit(false);
        w.c.set_gain(4.0);
        let ramped = w.shape(&chunk(1000));
        assert!(ramped[0] == 1000 && *ramped.last().unwrap() == 4000);
        assert_eq!(w.shape(&chunk(1000)), chunk(4000));
        assert_eq!(w.shape(&chunk(20000)), chunk(32767));
        assert_eq!(w.shape(&chunk(-20000)), chunk(-32768));
        w.c.set_gain(1.0);
        w.shape(&chunk(1000));
        assert_eq!(w.shape(&chunk(1000)), chunk(1000));
    }

    #[test]
    fn level_counts_the_gain_and_keeps_samples_whole_across_odd_reads() {
        let c = Controls::default();
        let mut m = Measure::default();
        c.set_gain(2.0);
        let data = bytes(&chunk(8000));
        m.measure(&c, &data[..3]);
        m.measure(&c, &data[3..]);
        let (peak, level, _) = c.take_level();
        assert!((peak - 16000.0 / 32767.0).abs() < 1e-6 && (level - peak).abs() < 1e-6);
        c.set_gain(8.0);
        c.set_limit(false);
        m.measure(&c, &bytes(&chunk(8000)));
        assert_eq!(c.take_level().0, 1.0);
    }

    pub fn wave(amp: f64, phase: usize) -> Vec<i16> {
        (phase..phase + CHUNK).map(|t| ((2.0 * PI * 440.0 * t as f64 / RATE as f64).sin() * amp) as i16).collect()
    }

    #[test]
    fn limiter_holds_loud_peaks_under_the_ceiling_then_lets_go_slowly() {
        let mut w = W::new();
        w.c.set_gain(4.0); // +12 dB on a wave at about -6 dBFS would clip hard
        let ceiling = limit_ceiling() * 32767.0;
        w.shape(&wave(16000.0, 0));
        let outs: Vec<Vec<i16>> = (1..20).map(|i| w.shape(&wave(16000.0, i * 480))).collect();
        let peak = |o: &Vec<i16>| o.iter().map(|&v| (v as i32).abs()).max().unwrap() as f32;
        assert!(outs.iter().all(|o| peak(o) <= ceiling * 10f32.powf(0.1 / 20.0))); // the 1 ms attack may overshoot a hair
        assert!(!outs.iter().any(|o| peak(o) >= 32767.0));
        w.shape(&wave(1000.0, 0)); // quiet again: the gain comes back over time, not at once
        assert!(w.s.limit_gain < 0.5);
        for _ in 0..200 {
            w.shape(&wave(1000.0, 0));
        }
        assert_eq!(w.s.limit_gain, 1.0);
    }

    #[test]
    fn quiet_audio_goes_through_untouched_with_the_limiter_on() {
        let mut w = W::new();
        let quiet = wave(3000.0, 0);
        assert_eq!(w.shape(&quiet), quiet);
    }

    // Automatic level

    /// A fixed pseudo-random normal noise.
    pub fn normal(n: usize, sigma: f64, seed: u64) -> Vec<f64> {
        let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        (0..n).map(|_| (-2.0 * next().ln()).sqrt() * (2.0 * PI * next()).cos() * sigma).collect()
    }

    fn talking(t: usize) -> bool {
        (t % (RATE * 4 / 10)) < RATE * 3 / 10
    }

    /// Speech-ish: 300 ms of tone at db (RMS dBFS), then 100 ms of room noise at pause_db.
    fn talk(seconds: f64, db: f64, pause_db: f64, seed: u64) -> Vec<f64> {
        let n = (seconds * RATE as f64) as usize;
        let noise = normal(n, 32767.0 * 10f64.powf(pause_db / 20.0), seed);
        (0..n)
            .map(|t| {
                if talking(t) {
                    (2.0 * PI * 220.0 * t as f64 / RATE as f64).sin() * 2f64.sqrt() * 32767.0 * 10f64.powf(db / 20.0)
                } else {
                    noise[t]
                }
            })
            .collect()
    }

    fn rms_db(x: &[f32]) -> f64 {
        20.0 * (rms(x) / 32767.0).log10()
    }

    fn speech_db(x: &[f32]) -> f64 {
        let talk: Vec<f32> = x.iter().enumerate().filter(|(t, _)| talking(*t)).map(|(_, v)| *v).collect();
        rms_db(&talk)
    }

    fn auto_w() -> W {
        let w = W::new();
        w.c.set_auto_level(true);
        w
    }

    #[test]
    fn automatic_level_brings_quiet_speech_up() {
        let mut w = auto_w();
        let out = w.shape_all(&talk(8.0, -34.0, -70.0, 1));
        assert!((speech_db(&out[out.len() - RATE * 2..]) - AUTO_TARGET_DB).abs() < 2.0);
    }

    #[test]
    fn automatic_level_brings_loud_speech_down() {
        let mut w = auto_w();
        let out = w.shape_all(&talk(4.0, -10.0, -70.0, 1));
        assert!((speech_db(&out[out.len() - RATE * 2..]) - AUTO_TARGET_DB).abs() < 2.0);
    }

    #[test]
    fn automatic_level_doesnt_turn_up_the_room_while_nobody_speaks() {
        let mut w = auto_w();
        w.shape_all(&talk(6.0, -26.0, -70.0, 1));
        let held = w.s.auto_db;
        w.shape_all(&talk(5.0, -48.0, -45.0, 1)); // only the room now, with a fan going
        assert!(w.s.auto_db <= held + 0.5);
    }

    #[test]
    fn startup_silence_does_not_make_room_noise_count_as_speech() {
        let mut w = auto_w();
        w.shape_all(&vec![0.0; 4 * 480]);
        w.shape_all(&normal(RATE * 5, 32767.0 * 10f64.powf(-45.0 / 20.0), 7));
        assert_eq!(w.s.auto_db, 0.0);
    }

    #[test]
    fn automatic_level_changes_gain_without_clicks() {
        let mut w = auto_w();
        let out = w.shape_all(&talk(3.0, -10.0, -70.0, 1));
        let tone_step = 2.0 * PI * 220.0 / RATE as f64 * 2f64.sqrt() * 32767.0 * 10f64.powf(-10.0 / 20.0);
        assert!((biggest_step(&out) as f64) < tone_step * 1.2); // never a jump bigger than the loudest tone makes
    }

    #[test]
    fn automatic_level_is_off_unless_asked_for() {
        let mut w = W::new();
        let quiet = talk(1.0, -34.0, -70.0, 1);
        let n = quiet.len() / 480 * 480;
        let expected: Vec<f32> = ints(&quiet[..n]).iter().map(|&v| v as f32).collect();
        assert_eq!(w.shape_all(&quiet), expected);
    }

    #[test]
    fn switching_automatic_level_off_fades_its_gain_out() {
        let mut w = auto_w();
        let talk = talk(6.0, -34.0, -70.0, 1);
        let before = w.shape_all(&talk);
        assert!(w.s.auto_db > 10.0);
        w.c.set_auto_level(false);
        let after = w.shape_all(&talk[..RATE]);
        assert_eq!(w.s.auto_db, 0.0);
        let tail = &before[before.len() - RATE..];
        let mut joined = tail.to_vec();
        joined.extend(&after);
        assert!(biggest_step(&joined) <= biggest_step(tail) * 1.05); // no jump down to the plain level
        let plain = ints(&talk[RATE / 2..RATE]);
        assert!(after[after.len() - RATE / 2..].iter().zip(plain).all(|(a, p)| (a - p as f32).abs() <= 1.0));
    }

    // Noise reduction (test_denoise.py)

    /// A denoiser that scales everything by gain and, like RNNoise, hands it back 20 ms late.
    struct Lagging {
        gain: f32,
        lag: Vec<f32>,
        seen: Arc<Mutex<Vec<Vec<f32>>>>,
        closed: Arc<Mutex<bool>>,
    }

    impl Denoise for Lagging {
        fn process(&mut self, samples: &[f32]) -> Vec<f32> {
            self.seen.lock().unwrap().push(samples.to_vec());
            let mut both = std::mem::take(&mut self.lag);
            both.extend_from_slice(samples);
            self.lag = both.split_off(samples.len());
            both.iter().map(|v| v * self.gain).collect()
        }
    }

    impl Drop for Lagging {
        fn drop(&mut self) {
            *self.closed.lock().unwrap() = true;
        }
    }

    #[derive(Clone, Default)]
    struct Made {
        count: Arc<Mutex<u32>>,
        seen: Arc<Mutex<Vec<Vec<f32>>>>,
        closed: Arc<Mutex<bool>>,
    }

    fn made(gain: f32) -> (Made, MakeDenoiser) {
        let m = Made::default();
        let m2 = m.clone();
        (m, Box::new(move || {
            *m2.count.lock().unwrap() += 1;
            Ok(Some(Box::new(Lagging { gain, lag: vec![0.0; denoise::DELAY], seen: m2.seen.clone(), closed: m2.closed.clone() })
                as Box<dyn Denoise>))
        }))
    }

    #[test]
    fn without_the_library_switching_it_on_leaves_the_audio_as_it_was() {
        let mut w = W::new();
        w.c.set_noise_reduction(true);
        assert_eq!(w.shape(&chunk(1000)), chunk(1000));
    }

    struct Caught;

    static LOGGED: Mutex<Vec<(std::thread::ThreadId, String)>> = Mutex::new(Vec::new());

    impl log::Log for Caught {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            LOGGED.lock().unwrap().push((std::thread::current().id(), record.args().to_string()));
        }
        fn flush(&self) {}
    }

    #[test]
    fn a_denoiser_that_fails_to_start_is_logged_once_and_the_audio_carries_on() {
        let calls = Arc::new(Mutex::new(0));
        let c2 = calls.clone();
        let mut w = W::with(Box::new(move || {
            *c2.lock().unwrap() += 1;
            Err("no".into())
        }));
        w.c.set_noise_reduction(true);
        let _ = log::set_logger(&Caught);
        log::set_max_level(log::LevelFilter::Error);
        assert_eq!(w.shape(&chunk(1000)), chunk(1000));
        assert_eq!(w.shape(&chunk(1000)), chunk(1000));
        assert_eq!(*calls.lock().unwrap(), 1);
        let me = std::thread::current().id();
        let logged = LOGGED.lock().unwrap().clone();
        assert_eq!(logged.iter().filter(|(who, text)| *who == me && text.contains("noise reduction")).count(), 1, "{logged:?}");
    }

    #[test]
    fn switching_in_a_pause_fades_over_one_chunk_each_way_and_lets_go_of_rnnoise() {
        let (m, make) = made(0.5);
        let mut w = W::with(make);
        w.c.set_limit(false);
        let hush = vec![200.0; CHUNK * 3]; // the room between words
        assert_eq!(w.shape_all(&hush), vec![200f32; CHUNK * 3]); // off: untouched
        w.c.set_noise_reduction(true);
        let fading = w.shape_all(&hush[..CHUNK]);
        assert!(fading[0] == 200.0 && (fading[CHUNK - 1] - 100.0).abs() <= 1.0);
        assert!(fading.windows(2).all(|p| p[1] <= p[0])); // smoothly, no jump
        assert_eq!(w.shape_all(&hush[..CHUNK]), vec![100f32; CHUNK]);
        w.c.set_noise_reduction(false);
        let back = w.shape_all(&hush[..CHUNK]);
        assert!(back[0] == 100.0 && (back[CHUNK - 1] - 200.0).abs() <= 1.0);
        assert_eq!(w.shape_all(&hush[..CHUNK]), vec![200f32; CHUNK]);
        assert!(*m.count.lock().unwrap() == 1 && *m.closed.lock().unwrap());
    }

    /// Loud speech in which no two neighbouring samples match, with silent pauses between.
    fn talk_nr(segments: usize, speech_chunks: usize, pause_chunks: usize) -> Vec<f64> {
        let (mut x, mut at) = (Vec::new(), 0usize);
        for _ in 0..segments {
            let n = CHUNK * speech_chunks;
            x.extend((at..at + n).map(|i| 1000.0 + (i % 20000) as f64));
            x.extend(vec![0.0; CHUNK * pause_chunks]);
            at += n;
        }
        x
    }

    #[test]
    fn switching_mid_sentence_waits_for_the_pause_so_no_speech_is_repeated_or_dropped() {
        let (_m, make) = made(1.0);
        let mut w = W::with(make);
        w.c.set_limit(false);
        let x = ints(&talk_nr(6, 30, 5));
        let mut out = Vec::new();
        for k in 0..x.len() / CHUNK {
            match k {
                10 | 80 => w.c.set_noise_reduction(true),
                45 | 120 => w.c.set_noise_reduction(false),
                _ => {}
            }
            if [31, 66, 101, 136].contains(&k) {
                // still in that sentence: nothing has changed yet
                assert_eq!(w.s.wet, if k == 31 || k == 101 { 0.0 } else { 1.0 }, "{k}");
            }
            if [35, 70, 105, 140].contains(&k) {
                // the next sentence: it switched in the pause
                assert_eq!(w.s.wet, if k == 35 || k == 105 { 1.0 } else { 0.0 }, "{k}");
            }
            out.extend(w.shape(&x[k * CHUNK..(k + 1) * CHUNK]));
        }
        let loud: Vec<i16> = out.into_iter().filter(|v| v.abs() > 500).collect();
        let speech: Vec<i16> = x.into_iter().filter(|&v| v > 500).collect();
        assert_eq!(loud, speech); // every sample of speech once, in order
    }

    #[test]
    fn with_no_pause_it_switches_anyway_after_two_seconds() {
        let (_m, make) = made(1.0);
        let mut w = W::with(make);
        w.c.set_limit(false);
        let wait = NR_SWITCH_WAIT as usize;
        let x = ints(&talk_nr(1, wait + 10, 0));
        w.c.set_noise_reduction(true);
        for k in 0..wait + 5 {
            w.shape(&x[k * CHUNK..(k + 1) * CHUNK]);
            assert_eq!(w.s.wet, if k + 1 >= wait { 1.0 } else { 0.0 });
        }
    }

    #[test]
    fn rnnoise_starts_on_the_audio_just_before_so_it_doesnt_open_with_silence() {
        let (m, make) = made(1.0);
        let mut w = W::with(make);
        w.c.set_limit(false);
        for level in [100, 200, 300] {
            w.shape(&chunk(level));
        }
        w.c.set_noise_reduction(true);
        let out = w.shape(&chunk(400));
        let firsts: Vec<f32> = m.seen.lock().unwrap().iter().map(|s| s[0]).collect();
        assert_eq!(firsts, vec![200.0, 300.0, 400.0]); // its 20 ms lookahead, then the chunk
        assert_eq!(*out.last().unwrap(), 200); // faded into what it gave back for 20 ms ago, not into silence
    }

    #[test]
    fn the_writer_lets_go_of_rnnoise_when_it_stops() {
        let (m, make) = made(1.0);
        let mut w = W::with(make);
        w.c.set_noise_reduction(true);
        w.shape(&chunk(1000));
        w.s.drop_denoiser();
        assert!(*m.closed.lock().unwrap() && !w.s.has_denoiser());
    }

    #[test]
    fn noise_reduction_comes_before_gain() {
        let (m, make) = made(0.5);
        let mut w = W::with(make);
        w.c.set_limit(false);
        w.c.set_noise_reduction(true);
        w.s.wet = 1.0; // already switched in
        w.c.set_gain(2.0);
        w.s.gain = 2.0; // already there, no fade
        let mut out = Vec::new();
        for _ in 0..3 {
            out = w.shape(&chunk(3000));
        }
        let seen = m.seen.lock().unwrap();
        assert_eq!(seen.last().unwrap().iter().cloned().fold(f32::MIN, f32::max), 3000.0); // it hears the mic as it comes
        assert!(out.iter().all(|&v| v == 3000)); // halved, then doubled
    }

    // The real RNNoise, built in

    /// A voice-like sound: a gliding pitch with its harmonics, in syllables, with a pause every half second.
    fn speech(seconds: f64) -> (Vec<f64>, Vec<bool>) {
        let n = (seconds * RATE as f64) as usize;
        let mut phase = 0.0;
        let (mut voice, mut talking) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for i in 0..n {
            let t = i as f64 / RATE as f64;
            phase += 2.0 * PI * (140.0 + 40.0 * (2.0 * PI * 0.7 * t).sin()) / RATE as f64;
            let v: f64 = (1..20).map(|k| (k as f64 * phase).sin() / k as f64).sum();
            let syllables = (2.0 * PI * 4.0 * t).sin().max(0.0).sqrt();
            let on = (t % 0.5) < 0.35;
            voice.push(v * syllables * if on { 1.0 } else { 0.0 } * 7000.0);
            talking.push(on);
        }
        (voice, talking)
    }

    /// A fan's broad hum and hiss with some key clicks on top.
    fn room(n: usize) -> Vec<f64> {
        let raw = normal(n, 1.0, 3);
        let mut out: Vec<f64> = (0..n)
            .map(|i| {
                let lo = i.saturating_sub(3);
                let hi = (i + 3).min(n);
                raw[lo..hi].iter().sum::<f64>() / 6.0 * 500.0 + (2.0 * PI * 100.0 * i as f64 / RATE as f64).sin() * 150.0
            })
            .collect();
        let clicks = normal(n / 9600 * 121, 1.0, 9);
        for (k, at) in (0..n / 9600).map(|k| (k, (k * 9601 + 777) % (n - 200))) {
            for j in 0..120 {
                out[at + j] += clicks[k * 121 + j] * 2500.0 * (-(j as f64) / 25.0).exp();
            }
        }
        out
    }

    #[test]
    fn real_rnnoise_takes_the_room_out_and_keeps_the_voice() {
        if denoise::load().is_none() {
            return; // RNNoise isn't installed here
        }
        let (voice, talking) = speech(6.0);
        let room = room(voice.len());
        let noisy: Vec<f64> = voice.iter().zip(&room).map(|(v, r)| v + r).collect();
        let mut w = W::with(Box::new(denoise::make));
        w.c.set_limit(false);
        w.c.set_noise_reduction(true);
        let out = w.shape_all(&noisy);
        let lag = denoise::DELAY;
        let at = |i: usize, back: usize| if i >= back { talking[i - back] } else { talking[talking.len() + i - back] };
        let late = |i: usize| i >= 2 * RATE; // give it a couple of seconds to learn the room
        let db = |xs: &mut dyn Iterator<Item = f64>| {
            let v: Vec<f64> = xs.collect();
            10.0 * (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).log10()
        };
        let pauses = |i: usize| !at(i, lag) && !at(i, lag + 1200) && late(i);
        let speaking = |i: usize| at(i, lag) && late(i);
        let out_p = db(&mut (0..out.len()).filter(|&i| pauses(i)).map(|i| out[i] as f64));
        let in_p = db(&mut (0..out.len()).filter(|&i| pauses(i)).map(|i| noisy[i]));
        let out_s = db(&mut (0..out.len()).filter(|&i| speaking(i)).map(|i| out[i] as f64));
        let in_s = db(&mut (0..out.len()).filter(|&i| speaking(i)).map(|i| noisy[i]));
        assert!(out_p < in_p - 15.0, "{out_p} {in_p}"); // the room drops by well over 15 dB between words
        assert!(out_s > in_s - 6.0, "{out_s} {in_s}"); // and the voice stays
    }

    #[test]
    fn real_rnnoise_keeps_up_with_10_ms_chunks_easily() {
        if denoise::load().is_none() {
            return;
        }
        let mut w = W::with(Box::new(denoise::make));
        w.c.set_noise_reduction(true);
        w.c.set_auto_level(true);
        let chunks: Vec<Vec<i16>> = (0..300).map(|i| ints(&normal(CHUNK, 3000.0, i))).collect();
        let started = std::time::Instant::now();
        for c in &chunks {
            w.shape(c);
        }
        // 300 chunks are 3 s of audio; even an unoptimised test build should take a small part of that
        assert!(started.elapsed().as_secs_f64() < 1.5, "{:?}", started.elapsed());
    }
}
