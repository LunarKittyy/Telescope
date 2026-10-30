//! The jitter buffer (audio.py's JitterBuffer): holds the phone's audio for the writer, playing up to MAX_SKEW faster
//! or slower to stay near the target, skipping quiet moments when far behind, and learning how unevenly audio arrives.

use std::collections::VecDeque;

pub const RATE: usize = 48_000;
pub const BYTES_PER_MS: usize = RATE * 2 / 1000;
/// Samples the writer hands the output at a time (10 ms).
pub const CHUNK: usize = 10 * RATE / 1000;
pub const TARGET_MS: usize = 60;
/// Where the buffer starts on a steady connection; it grows when audio arrives unevenly.
pub const USB_TARGET_MS: usize = 20;
pub const WIFI_TARGET_MS: usize = 40;
/// Jump back to the target once this far behind (more than playing faster can catch up).
pub const MAX_MS: usize = 150;
pub const MAX_TARGET_MS: usize = 120;
/// Past MAX_MS, while nothing reads the output.
pub const MAX_BACKLOG_MS: usize = 500;
pub const JITTER_MARGIN_MS: f64 = 10.0;
/// Playing 0.2% faster or slower can't be heard.
pub const MAX_SKEW: f64 = 0.002;
/// Full skew 10 ms off target.
pub const SKEW_PER_SAMPLE: f64 = MAX_SKEW / (10.0 * RATE as f64 / 1000.0);
/// Far over target and never quiet (music, a fan): still hard to hear, and drains 100 ms in 20 s.
pub const FAST_SKEW: f64 = 0.005;
/// Over the target by this much, the skew can grow toward FAST_SKEW...
pub const FAST_AFTER_MS: f64 = 30.0;
/// ...reaching it here.
pub const FAST_FULL_MS: f64 = 60.0;
/// This far over target, a quiet moment is skipped with a crossfade instead.
pub const QUIET_SKIP_MS: f64 = 20.0;
/// Always quiet enough to skip...
pub const QUIET_DBFS: f64 = -50.0;
/// ...never when louder (speech), and in between only near the room's own noise.
pub const QUIET_MAX_DBFS: f64 = -35.0;
pub const AVERAGE_S: f64 = 0.5;
/// Samples (2 ms) to fade out and back in around a gap or a jump.
pub const FADE: usize = 96;
pub const LEAD_WINDOW_S: i64 = 10;
/// How quickly a late burst stops counting.
pub const LATE_FORGET_S: f64 = 5.0;
pub const FULL_SCALE: f32 = 32767.0;

/// The noise floor estimate creeps up 1 dB/s, so it follows a noisier room.
fn floor_rise() -> f64 {
    10f64.powf(1.0 / 20.0 / 100.0)
}

pub fn dbfs(db: f64) -> f64 {
    FULL_SCALE as f64 * 10f64.powf(db / 20.0)
}

/// numpy's linspace(a, b, k) as float32.
pub fn linspace(a: f64, b: f64, k: usize) -> Vec<f32> {
    match k {
        0 => Vec::new(),
        1 => vec![a as f32],
        _ => (0..k).map(|i| (a + (b - a) * i as f64 / (k - 1) as f64) as f32).collect(),
    }
}

pub fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Little-endian 16-bit samples, keeping a trailing odd byte for the next call.
pub fn take_samples(odd: &mut Vec<u8>, data: &[u8]) -> Vec<f32> {
    let mut all = std::mem::take(odd);
    all.extend_from_slice(data);
    let whole = all.len() - all.len() % 2;
    let samples = all[..whole].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes([b[0], b[1]]) as f32).collect();
    *odd = all[whole..].to_vec();
    samples
}

pub type Clock = Box<dyn Fn() -> f64 + Send>;

pub fn monotonic() -> f64 {
    telescope_core::time::monotonic_ns() as f64 / 1e9
}

pub struct JitterBuffer {
    /// The target with a steady connection, in samples.
    base: i64,
    target: i64,
    /// How far over target before jumping back.
    max_extra: i64,
    adaptive: bool,
    clock: Clock,
    buf: Vec<f32>,
    /// Read position in buf; the sample before it stays for the interpolation.
    pos: f64,
    odd: Vec<u8>,
    /// False until the target has built up, and again after running dry.
    playing: bool,
    /// How full it is on average, in samples.
    avg: f64,
    /// (second, earliest arrival relative to the audio's own clock).
    leads: VecDeque<(i64, f64)>,
    /// How late audio has been arriving recently, in samples.
    late: f64,
    received: f64,
    last_arrival: f64,
    /// How many times it ran out and faded to silence.
    pub ran_dry: u32,
    /// The quietest it's been lately (RMS per chunk).
    floor: f64,
    /// The next audio pushed starts another connection's: fade it in.
    fade_next: bool,
    /// The last pop was a whole chunk of recorded audio, rather than refill silence.
    pub has_audio: bool,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        JitterBuffer::new(TARGET_MS, MAX_MS, Box::new(monotonic), true)
    }
}

impl JitterBuffer {
    pub fn new(target_ms: usize, max_ms: usize, clock: Clock, adaptive: bool) -> JitterBuffer {
        let base = (target_ms * RATE / 1000) as i64;
        JitterBuffer {
            base,
            target: base,
            max_extra: (max_ms as i64 - target_ms as i64) * RATE as i64 / 1000,
            adaptive,
            clock,
            buf: vec![0.0],
            pos: 1.0,
            odd: Vec::new(),
            playing: false,
            avg: 0.0,
            leads: VecDeque::new(),
            late: 0.0,
            received: 0.0,
            last_arrival: 0.0,
            ran_dry: 0,
            floor: FULL_SCALE as f64,
            fade_next: false,
            has_audio: false,
        }
    }

    pub fn with_target(target_ms: usize) -> JitterBuffer {
        JitterBuffer::new(target_ms, MAX_MS, Box::new(monotonic), true)
    }

    /// The connection changed: learn its jitter afresh, with the appropriate USB or Wi-Fi minimum.
    pub fn set_target_ms(&mut self, target_ms: usize) {
        let base = (target_ms * RATE / 1000) as i64;
        self.max_extra = (self.max_extra + self.base - base).max(0);
        self.base = base;
        self.target = base;
        self.late = 0.0;
        self.leads.clear();
        self.received = 0.0;
        self.last_arrival = 0.0;
    }

    pub fn push(&mut self, data: &[u8]) {
        let now = (self.clock)();
        let mut samples = take_samples(&mut self.odd, data);
        if samples.is_empty() {
            return;
        }
        if self.fade_next {
            self.fade_next = false;
            let k = FADE.min(samples.len());
            for (s, w) in samples.iter_mut().zip(linspace(0.0, 1.0, k)) {
                *s *= w;
            }
        }
        let n = samples.len();
        self.buf.extend_from_slice(&samples);
        self.track_jitter(now); // by when this chunk's first sample arrived
        self.received += n as f64;
        let hard = self.pos as i64 + self.target + self.max_extra + (MAX_BACKLOG_MS * RATE / 1000) as i64;
        if self.buf.len() as i64 > hard {
            // The writer stalled (an idle output stops reading): never pile up more than this
            let drop = (self.buf.len() as i64 - hard) as usize;
            self.buf.drain(..drop);
            self.pos = (self.pos - drop as f64).max(1.0);
        }
    }

    /// A new connection: its audio's timing starts over, so the old arrivals say nothing about it.
    pub fn restart_clock(&mut self) {
        self.leads.clear();
        self.received = 0.0;
        self.odd.clear();
    }

    /// The audio moves to another connection: fade out what's here and fade in what comes next, since the two don't
    /// line up to the sample.
    pub fn splice(&mut self) {
        let left = (self.buf.len() as f64 - self.pos) as i64;
        let k = (FADE as i64).min(left);
        if k > 0 {
            let k = k as usize;
            let start = self.buf.len() - k;
            for (s, w) in self.buf[start..].iter_mut().zip(linspace(1.0, 0.0, k)) {
                *s *= w;
            }
        }
        self.fade_next = true;
        self.leads.clear();
        self.received = 0.0;
        self.odd.clear();
    }

    fn track_jitter(&mut self, now: f64) {
        let lead = now * RATE as f64 - self.received; // the later audio arrives, the bigger this gets
        // The earliest arrival over the last LEAD_WINDOW_S, kept per second: audio lost on the way (a mic that
        // reopened, a backlog the phone dropped) shifts this for good, and a window lets that go
        let second = now as i64;
        match self.leads.back_mut() {
            Some(last) if last.0 == second => last.1 = last.1.min(lead),
            _ => self.leads.push_back((second, lead)),
        }
        while self.leads.front().is_some_and(|l| l.0 <= second - LEAD_WINDOW_S) {
            self.leads.pop_front();
        }
        let earliest = self.leads.iter().map(|l| l.1).fold(f64::INFINITY, f64::min);
        let dt = if self.last_arrival != 0.0 { (now - self.last_arrival).max(0.0) } else { 0.0 };
        self.last_arrival = now;
        if !self.adaptive {
            return;
        }
        self.late = (lead - earliest).max(self.late * (-dt / LATE_FORGET_S).exp());
        let wanted = (self.base as f64)
            .max(self.late + JITTER_MARGIN_MS * RATE as f64 / 1000.0)
            .min(MAX_TARGET_MS as f64 * RATE as f64 / 1000.0);
        self.target = wanted as i64;
    }

    /// m samples of audio, or silence while (re)filling.
    pub fn pop(&mut self, m: usize) -> Vec<i16> {
        self.has_audio = false;
        let fill = self.buf.len() as f64 - self.pos;
        let target = self.target as f64;
        let fade_in = if !self.playing {
            if fill < target {
                return vec![0; m];
            }
            self.playing = true;
            self.avg = fill;
            true
        } else {
            false
        };
        self.avg += (fill - self.avg) * (m as f64 / (AVERAGE_S * RATE as f64)).min(1.0);
        let over_ms = (self.avg - target) * 1000.0 / RATE as f64;
        let fast = ((over_ms - FAST_AFTER_MS) / (FAST_FULL_MS - FAST_AFTER_MS)).clamp(0.0, 1.0);
        let ratio = 1.0 + ((self.avg - target) * SKEW_PER_SAMPLE).clamp(-MAX_SKEW, MAX_SKEW + (FAST_SKEW - MAX_SKEW) * fast);
        let after = fill - m as f64 * ratio;
        let mut skip = 0i64;
        if after > target + self.max_extra as f64 {
            skip = (after - target) as i64; // far too much: jump back to the target
            self.avg = target;
        } else if after > target && over_ms > QUIET_SKIP_MS && self.quiet(self.pos as i64, (fill - target) as i64) {
            skip = (after - target) as i64; // a pause: jumping back to the target can't be heard
            self.avg = target;
        }
        let at: Vec<f64> = (0..m).map(|i| self.pos + skip as f64 + ratio * i as f64).collect();
        let limit = self.buf.len() as f64 - 2.0; // cubic needs two samples ahead
        let usable = at.partition_point(|&a| a < limit);
        self.has_audio = usable == m;
        let mut out = self.cubic(&at[..usable]);
        if skip != 0 {
            let k = FADE.min(usable);
            let before_at: Vec<f64> = (0..k).map(|i| self.pos + ratio * i as f64).collect();
            let before = self.cubic(&before_at);
            for (i, w) in linspace(0.0, 1.0, k).into_iter().enumerate() {
                out[i] = before[i] * (1.0 - w) + out[i] * w;
            }
        }
        if fade_in {
            let k = FADE.min(out.len());
            for (s, w) in out.iter_mut().zip(linspace(0.0, 1.0, k)) {
                *s *= w;
            }
        }
        if !out.is_empty() {
            self.floor = rms(&out).min(self.floor * floor_rise());
        }
        if usable < m {
            // Ran dry: fade out what's left instead of stopping dead, then fill up to the target again
            let k = FADE.min(out.len());
            let start = out.len() - k;
            for (s, w) in out[start..].iter_mut().zip(linspace(1.0, 0.0, k)) {
                *s *= w;
            }
            let last = *self.buf.last().unwrap_or(&0.0);
            self.buf = vec![last];
            self.pos = 1.0;
            self.playing = false;
            self.ran_dry += 1;
            out.resize(m, 0.0);
        } else {
            let end = self.pos + skip as f64 + ratio * m as f64;
            let keep_from = end as i64 - 1;
            self.buf.drain(..keep_from as usize);
            self.pos = end - keep_from as f64;
        }
        out.iter().map(|&v| v.round_ties_even().clamp(-32768.0, FULL_SCALE) as i16).collect()
    }

    /// Whether the n samples from start (what a skip would leave out, and a bit after) are a quiet moment.
    fn quiet(&self, start: i64, n: i64) -> bool {
        let len = self.buf.len() as i64;
        let (a, b) = (start.clamp(0, len), (start + n).clamp(0, len));
        if b <= a {
            return false;
        }
        rms(&self.buf[a as usize..b as usize]) < self.quiet_limit()
    }

    /// The RMS a quiet moment stays under: near the room's own noise, and never as loud as speech.
    pub fn quiet_limit(&self) -> f64 {
        dbfs(QUIET_MAX_DBFS).min(dbfs(QUIET_DBFS).max(self.floor * 2.0))
    }

    /// Catmull-Rom between samples: straight lines would dull "s" and "t" sounds.
    fn cubic(&self, at: &[f64]) -> Vec<f32> {
        let x = &self.buf;
        at.iter()
            .map(|&a| {
                let i = a.floor() as i64;
                let t = (a - i as f64) as f32;
                let i = i as usize;
                let (p0, p1, p2, p3) = (x[i.saturating_sub(1)], x[i], x[i + 1], x[i + 2]);
                p1 + 0.5 * t * (p2 - p0 + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + t * (3.0 * (p1 - p2) + p3 - p0)))
            })
            .collect()
    }

    pub fn buffered_ms(&self) -> i64 {
        (self.buf.len() as f64 - self.pos) as i64 * 1000 / RATE as i64
    }

    pub fn target_ms(&self) -> i64 {
        self.target * 1000 / RATE as i64
    }
}

/// USB arrives evenly enough for a shorter buffer than Wi-Fi.
pub fn target_for(url: &str) -> usize {
    let host = url.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    let host = host.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = if let Some(v6) = host.strip_prefix('[') { v6.split(']').next().unwrap_or("") } else { host.split(':').next().unwrap_or("") };
    if host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") { USB_TARGET_MS } else { WIFI_TARGET_MS }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub fn pcm(samples: &[f64]) -> Vec<u8> {
        samples.iter().flat_map(|&s| (s.round_ties_even().clamp(-32768.0, 32767.0) as i16).to_le_bytes()).collect()
    }

    pub fn sine(n: usize, start: usize, freq: f64, amp: f64) -> Vec<f64> {
        (0..n).map(|i| amp * (2.0 * std::f64::consts::PI * freq * (i + start) as f64 / RATE as f64).sin()).collect()
    }

    pub fn full(n: usize, v: f64) -> Vec<f64> {
        vec![v; n]
    }

    #[derive(Clone, Default)]
    pub struct TestClock(Arc<Mutex<f64>>);

    impl TestClock {
        pub fn add(&self, dt: f64) {
            *self.0.lock().unwrap() += dt;
        }
        pub fn clock(&self) -> Clock {
            let c = self.0.clone();
            Box::new(move || *c.lock().unwrap())
        }
    }

    fn jb(target: usize, max: usize, clock: &TestClock, adaptive: bool) -> JitterBuffer {
        JitterBuffer::new(target, max, clock.clock(), adaptive)
    }

    fn to_f(x: &[i16]) -> Vec<f32> {
        x.iter().map(|&v| v as f32).collect()
    }

    pub fn biggest_step(x: &[f32]) -> f32 {
        x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
    }

    /// The phone pushes 10 ms chunks at phone_speed while the writer pops 10 ms; what came out and the fill each pop.
    fn steady(jb: &mut JitterBuffer, clock: &TestClock, seconds: f64, speed: f64, freq: f64) -> (Vec<f32>, Vec<i64>) {
        let (mut out, mut fills, mut sent, mut owed) = (Vec::new(), Vec::new(), 0usize, 0.0f64);
        for _ in 0..(seconds * 100.0) as usize {
            clock.add(0.01);
            owed += CHUNK as f64 * speed;
            let n = owed as usize - sent;
            jb.push(&pcm(&sine(n, sent, freq, 10000.0)));
            sent += n;
            out.extend(to_f(&jb.pop(CHUNK)));
            fills.push(jb.buffered_ms());
        }
        (out, fills)
    }

    fn feed(jb: &mut JitterBuffer, clock: &TestClock, seconds: f64, make: &dyn Fn(usize, usize) -> Vec<f64>) -> (Vec<f32>, Vec<i64>) {
        let (mut out, mut fills, mut sent) = (Vec::new(), Vec::new(), 0usize);
        for _ in 0..(seconds * 100.0) as usize {
            clock.add(0.01);
            jb.push(&pcm(&make(CHUNK, sent)));
            sent += CHUNK;
            out.extend(to_f(&jb.pop(CHUNK)));
            fills.push(jb.buffered_ms());
        }
        (out, fills)
    }

    /// A quiet room, about -60 dBFS (a fixed pseudo-random noise).
    fn room(n: usize, start: usize) -> Vec<f64> {
        let mut x = (start as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (0..n)
            .map(|_| {
                let mut g = 0.0;
                for _ in 0..4 {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    g += (x >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
                }
                g * 30.0 * (12f64 / 4.0).sqrt()
            })
            .collect()
    }

    const MS: usize = RATE / 1000;

    #[test]
    fn jitter_buffer_waits_for_the_target_then_fades_in_and_plays_in_order() {
        let c = TestClock::default();
        let mut jb = jb(20, MAX_MS, &c, false);
        jb.push(&pcm(&full(10 * MS, 1000.0)));
        assert_eq!(jb.pop(2), vec![0, 0]); // still filling: silence
        jb.push(&pcm(&full(10 * MS, 1000.0)));
        let out = jb.pop(CHUNK);
        assert!(out[0] == 0 && out[FADE / 2] < 1000); // fading in, not starting at full level
        assert!(out[FADE..].iter().all(|&v| v == 1000));
        assert!((9..=10).contains(&jb.buffered_ms()));
    }

    #[test]
    fn jitter_buffer_fades_out_when_it_runs_dry_and_refills() {
        let c = TestClock::default();
        let mut jb = jb(10, MAX_MS, &c, false);
        jb.push(&pcm(&full(15 * MS, 1000.0)));
        jb.pop(CHUNK);
        let out = jb.pop(CHUNK); // only 5 ms left
        let played: Vec<i16> = out.iter().copied().take_while(|&v| v != 0).collect();
        assert!(!played.is_empty() && *played.last().unwrap() < 100); // faded down, not cut off at 1000
        assert!(out[played.len()..].iter().all(|&v| v == 0));
        assert_eq!(jb.pop(2), vec![0, 0]); // refilling again
    }

    #[test]
    fn jitter_buffer_holds_the_target_when_the_phones_clock_runs_fast_or_slow() {
        for speed in [1.001, 0.999] {
            let c = TestClock::default();
            let mut jb = jb(40, MAX_MS, &c, false);
            let (out, fills) = steady(&mut jb, &c, 120.0, speed, 440.0);
            let settled = &fills[1000..];
            assert!(20 <= *settled.iter().min().unwrap() && *settled.iter().max().unwrap() <= 40, "{speed}");
            assert!(biggest_step(&out[5000..]) < 650.0, "{speed}"); // a 440 Hz sine at 10000 moves at most ~575 a sample
        }
    }

    #[test]
    fn jitter_buffer_drift_correction_keeps_high_notes_sharp() {
        let c = TestClock::default();
        let mut jb = jb(40, MAX_MS, &c, false);
        let (out, _) = steady(&mut jb, &c, 10.0, 1.002, 8000.0);
        assert!(out[RATE * 5..].iter().fold(0.0f32, |m, v| m.max(v.abs())) > 9500.0); // straight lines would lose a lot of this
    }

    #[test]
    fn jitter_buffer_fades_back_to_the_target_when_far_behind() {
        let c = TestClock::default();
        let mut jb = jb(20, 60, &c, false);
        let (out, _) = steady(&mut jb, &c, 1.0, 1.0, 440.0);
        jb.push(&pcm(&sine(200 * MS, 100 * CHUNK, 440.0, 10000.0))); // 200 ms arrive at once
        let after = to_f(&jb.pop(CHUNK));
        assert!(jb.buffered_ms() <= 25);
        let mut joined = out[out.len() - 10..].to_vec();
        joined.extend(after);
        assert!(biggest_step(&joined) < 1500.0); // crossfaded, not a jump
    }

    #[test]
    fn jitter_buffer_grows_the_target_when_audio_arrives_unevenly_then_shrinks_again() {
        let c = TestClock::default();
        let mut jb = jb(20, MAX_MS, &c, true);
        steady(&mut jb, &c, 2.0, 1.0, 440.0);
        assert_eq!(jb.target_ms(), 20);
        c.add(0.08); // 80 ms with nothing, then it all arrives
        jb.push(&pcm(&sine(80 * MS, 0, 440.0, 10000.0)));
        assert!(jb.target_ms() >= 70); // 70 ms later than the rest, plus a margin
        steady(&mut jb, &c, 30.0, 1.0, 440.0);
        assert!(jb.target_ms() < 40);
    }

    #[test]
    fn a_short_pause_after_a_burst_never_wraps_back_to_the_end_of_the_buffer() {
        let mut jb = JitterBuffer::with_target(40);
        let mut x = sine(105 * MS, 0, 440.0, 10000.0);
        let n = 50 * MS;
        x.extend((0..n).map(|i| 10.0 + 90.0 * i as f64 / (n - 1) as f64));
        jb.push(&pcm(&x));
        let out: Vec<i32> = (0..14).flat_map(|_| jb.pop(CHUNK)).map(i32::from).collect();
        // All a slowly rising, quiet ramp here. A negative catch-up skip would read the tail first, then jump back.
        let part = &out[110 * MS..130 * MS];
        assert!(part.windows(2).map(|w| w[1] - w[0]).min().unwrap() >= -1);
    }

    #[test]
    fn audio_lost_on_the_way_doesnt_keep_the_target_up() {
        let c = TestClock::default();
        let mut jb = jb(20, MAX_MS, &c, true);
        steady(&mut jb, &c, 2.0, 1.0, 440.0);
        c.add(1.3); // the phone's mic reopened: 1.3 s of audio that never comes
        jb.push(&pcm(&sine(480, 0, 440.0, 10000.0)));
        assert_eq!(jb.target_ms(), MAX_TARGET_MS as i64);
        steady(&mut jb, &c, 30.0, 1.0, 440.0);
        assert!(jb.target_ms() < 30);
    }

    #[test]
    fn jitter_buffer_caps_what_piles_up_while_nothing_reads() {
        let c = TestClock::default();
        let mut jb = jb(20, MAX_MS, &c, false);
        for _ in 0..300 {
            jb.push(&pcm(&full(CHUNK, 0.0)));
        }
        assert!(jb.buffered_ms() <= (20 + 130 + MAX_BACKLOG_MS) as i64);
    }

    #[test]
    fn usb_starts_with_a_shorter_buffer_than_wifi() {
        assert_eq!(target_for("https://127.0.0.1:5000/v1/audio"), USB_TARGET_MS);
        assert_eq!(target_for("https://192.168.1.20:8443/v1/audio"), WIFI_TARGET_MS);
        assert_eq!(target_for("browser:/audio"), WIFI_TARGET_MS);
    }

    #[test]
    fn moving_to_usb_lowers_the_buffer_target_without_discarding_audio() {
        let c = TestClock::default();
        let mut jb = jb(40, MAX_MS, &c, false);
        jb.push(&pcm(&full(60 * MS, 1000.0)));
        let before = jb.buffered_ms();
        jb.set_target_ms(20);
        assert!(jb.target_ms() == 20 && jb.buffered_ms() == before);
        assert!(*jb.pop(CHUNK).iter().max().unwrap() > 0);
        jb.set_target_ms(40);
        assert_eq!(jb.target_ms(), 40);
    }

    #[test]
    fn extra_delay_goes_in_the_next_pause() {
        let c = TestClock::default();
        let mut jb = jb(40, 200, &c, false);
        feed(&mut jb, &c, 2.0, &room);
        jb.push(&pcm(&room(100 * MS, 1_000_000))); // a Wi-Fi burst: 100 ms more than it needs
        let (_, fills) = feed(&mut jb, &c, 1.0, &room);
        assert!(*fills.last().unwrap() <= 45); // skipped within about the half second its average takes
    }

    #[test]
    fn extra_delay_under_constant_sound_drains_by_playing_faster_without_jumps() {
        let c = TestClock::default();
        let mut jb = jb(40, 200, &c, false);
        let s = |n, st| sine(n, st, 440.0, 10000.0);
        feed(&mut jb, &c, 2.0, &s);
        jb.push(&pcm(&sine(100 * MS, 200 * CHUNK, 440.0, 10000.0)));
        let (out, fills) = feed(&mut jb, &c, 40.0, &|n, st| sine(n, st + 100 * MS, 440.0, 10000.0));
        assert!(fills[200] > 100); // never skipped: it's never quiet
        assert!(*fills[fills.len() - 200..].iter().max().unwrap() <= 50);
        assert!(biggest_step(&out[RATE..]) < 650.0); // sped up smoothly, no jumps
    }

    #[test]
    fn speech_is_never_skipped() {
        let c = TestClock::default();
        let mut jb = jb(40, 200, &c, false);
        let speech = |n, st| sine(n, st, 300.0, 1500.0); // about -30 dBFS
        feed(&mut jb, &c, 2.0, &speech);
        jb.push(&pcm(&speech(60 * MS, 200 * CHUNK)));
        let (_, fills) = feed(&mut jb, &c, 2.0, &speech);
        assert!(*fills.iter().min().unwrap() > 70);
    }

    #[test]
    fn a_splice_fades_out_the_old_connection_and_in_the_new_one() {
        let c = TestClock::default();
        let mut jb = jb(20, MAX_MS, &c, false);
        jb.push(&pcm(&full(30 * MS, 8000.0)));
        jb.splice();
        jb.push(&pcm(&full(30 * MS, -8000.0))); // where the other route's audio happens to start
        let out: Vec<f32> = (0..5).flat_map(|_| to_f(&jb.pop(CHUNK))).collect();
        assert!(biggest_step(&out[FADE..]) < 400.0); // through zero in 2 ms steps, not a 16000 jump
    }
}
