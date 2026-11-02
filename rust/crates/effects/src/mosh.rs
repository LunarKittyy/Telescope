//! Datamosh on H.264 access units (h264_mosh.py): keyframes dropped, frames repeated, and the headers fixed up so the decoder takes it.
//!
//! Dropping a keyframe alone doesn't mosh: the decoder throws away the frames after it, since their frame numbers and picture order
//! counts don't follow on from the last frame it showed. So once a frame has been dropped or repeated, every slice header gets
//! frame_num and pic_order_cnt_lsb that continue from the last frame decoded, and the new motion lands on the old picture. Only the
//! first bytes of each slice are touched; the rest is copied as it came.

use std::sync::{Arc, Mutex};
use telescope_core::host::{PacketStep, PacketStepFactory};

const SLICE: u8 = 1;
const IDR: u8 = 5;
const SPS: u8 = 7;
/// Enough escaped bytes for a slice header up to pic_order_cnt_lsb
const HEAD: usize = 24;
const HIGH_PROFILES: [u32; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
const START: [u8; 4] = [0, 0, 0, 1];

/// What a Mosher does, swapped whole from the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mosh {
    pub drop_keyframes: bool,
    /// Extra copies of every frame, which smears the motion further
    pub repeats: u32,
}

impl Default for Mosh {
    fn default() -> Mosh {
        Mosh { drop_keyframes: true, repeats: 0 }
    }
}

/// (start, end) of each NAL unit's bytes after its start code.
fn nals(data: &[u8]) -> Vec<(usize, usize)> {
    let starts: Vec<usize> = (0..data.len().saturating_sub(2)).filter(|&i| data[i..i + 3] == [0, 0, 1]).map(|i| i + 3).collect();
    // matches don't overlap, as a regex's don't
    let mut kept: Vec<usize> = Vec::new();
    for s in starts {
        if kept.last().is_none_or(|&l| s >= l + 3) {
            kept.push(s);
        }
    }
    kept.iter().enumerate().map(|(i, &s)| {
        let mut e = if i + 1 < kept.len() { kept[i + 1] - 3 } else { data.len() };
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        (s, e)
    }).collect()
}

fn unescape(b: &[u8]) -> Vec<u8> {
    let (mut out, mut zeros) = (Vec::with_capacity(b.len()), 0);
    for &x in b {
        if zeros >= 2 && x == 3 {
            zeros = 0;
            continue;
        }
        out.push(x);
        zeros = if x == 0 { zeros + 1 } else { 0 };
    }
    out
}

fn escape(b: &[u8]) -> Vec<u8> {
    let (mut out, mut zeros) = (Vec::with_capacity(b.len() + 4), 0);
    for &x in b {
        if zeros >= 2 && x <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(x);
        zeros = if x == 0 { zeros + 1 } else { 0 };
    }
    out
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn u(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = *self.data.get(self.pos >> 3)?;
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u64;
            self.pos += 1;
        }
        Some(v as u32)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1 + self.u(zeros)? as u64) as u32)
    }

    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k & 1 == 1 { (k + 1) / 2 } else { -(k / 2) } as i32)
    }
}

fn put(buf: &mut [u8], pos: usize, n: u32, v: u32) {
    for i in 0..n as usize {
        let p = pos + i;
        let bit = 1u8 << (7 - (p & 7));
        if (v >> (n as usize - 1 - i)) & 1 == 1 {
            buf[p >> 3] |= bit;
        } else {
            buf[p >> 3] &= !bit;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Sps {
    frame_num_bits: u32,
    poc_type: u32,
    poc_bits: u32,
    frame_mbs_only: bool,
}

/// The fields a slice header needs, or None for POC type 1, which this doesn't rewrite.
fn parse_sps(nal: &[u8]) -> Option<Sps> {
    let data = unescape(nal);
    let mut b = Bits { data: &data, pos: 8 };
    let profile = b.u(8)?;
    b.u(16)?;
    b.ue()?;
    if HIGH_PROFILES.contains(&profile) {
        let chroma = b.ue()?;
        if chroma == 3 {
            b.u(1)?;
        }
        b.ue()?;
        b.ue()?;
        b.u(1)?;
        if b.u(1)? == 1 {
            for i in 0..if chroma != 3 { 8 } else { 12 } {
                if b.u(1)? == 1 {
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..if i < 6 { 16 } else { 64 } {
                        if next != 0 {
                            next = (last + b.se()? + 256).rem_euclid(256);
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    let frame_num_bits = b.ue()? + 4;
    let poc_type = b.ue()?;
    let mut poc_bits = 0;
    if poc_type == 0 {
        poc_bits = b.ue()? + 4;
    } else if poc_type == 1 {
        return None;
    }
    b.ue()?;
    b.u(1)?;
    b.ue()?;
    b.ue()?;
    Some(Sps { frame_num_bits, poc_type, poc_bits, frame_mbs_only: b.u(1)? == 1 })
}

/// Where to split a slice so its head holds the header and no escape sequence straddles the split.
fn cut(nal: &[u8]) -> usize {
    let mut at = HEAD.min(nal.len());
    while at < nal.len() && at >= 2 && (nal[at - 1] == 0 || nal[at - 2] == 0) {
        at += 1;
    }
    at
}

/// A packet step that moshes one stream. settings() is read for each access unit. clean_up: let the next keyframe through first, so
/// the picture comes back.
pub struct Mosher {
    settings: Arc<Mutex<Mosh>>,
    sps: Option<Sps>,
    sps_bytes: Vec<u8>,
    /// Once a frame is dropped or repeated, every header after it gets new numbers
    rewriting: bool,
    /// Nothing to smear onto before the first frame
    decoded: bool,
    clean_up: bool,
    /// The last reference frame's, as decoded
    frame_num: u32,
    /// The last frame's, as decoded
    poc: u32,
    poc_step: u32,
    /// The last frame's as the phone sent it
    sent_poc: Option<u32>,
}

/// frame_num and pic_order_cnt_lsb's bit positions and values in an unescaped slice head.
type Fields = (usize, u32, Option<usize>, u32);

impl Mosher {
    pub fn new(settings: Arc<Mutex<Mosh>>, clean_up: bool) -> Mosher {
        Mosher { settings, sps: None, sps_bytes: Vec::new(), rewriting: false, decoded: false, clean_up, frame_num: 0, poc: 0, poc_step: 2, sent_poc: None }
    }

    fn mod_poc(&self) -> u64 {
        1u64 << self.sps.map_or(1, |s| s.poc_bits.max(1))
    }

    fn process(&mut self, unit: &[u8]) -> Option<Vec<Vec<u8>>> {
        let mosh = *self.settings.lock().unwrap();
        let nals = nals(unit);
        let types: Vec<u8> = nals.iter().map(|&(s, _)| unit[s] & 31).collect();
        if let Some(i) = types.iter().position(|&t| t == SPS) {
            let (s, e) = nals[i];
            if unit[s..e] != self.sps_bytes[..] {
                self.sps_bytes = unit[s..e].to_vec();
                self.sps = parse_sps(&unit[s..e]);
                // a new size or format: this keyframe has to go through
                return Some(self.pass(unit, &nals, &types));
            }
        }
        if self.sps.is_none() || !types.iter().any(|&t| t == SLICE || t == IDR) {
            return Some(vec![unit.to_vec()]);
        }
        if types.contains(&IDR) {
            if !mosh.drop_keyframes || !self.decoded || self.clean_up {
                self.clean_up = false;
                return Some(self.pass(unit, &nals, &types));
            }
            let mut kept = Vec::new();
            for (&(s, e), &t) in nals.iter().zip(&types) {
                if t != SLICE && t != IDR {
                    kept.extend_from_slice(&START);
                    kept.extend_from_slice(&unit[s..e]);
                }
            }
            self.rewriting = true;
            return Some(if kept.is_empty() { Vec::new() } else { vec![kept] });
        }
        if mosh.repeats > 0 {
            self.rewriting = true;
        }
        (0..=mosh.repeats).map(|i| self.renumber(unit, &nals, &types, i)).collect()
    }

    /// A keyframe let through: the numbers start again from it.
    fn pass(&mut self, unit: &[u8], nals: &[(usize, usize)], types: &[u8]) -> Vec<Vec<u8>> {
        self.rewriting = false;
        self.decoded = true;
        if types.contains(&IDR) {
            self.note(unit, nals, types);
        }
        vec![unit.to_vec()]
    }

    /// Remember a frame's numbers as decoded, unchanged.
    fn note(&mut self, unit: &[u8], nals: &[(usize, usize)], types: &[u8]) -> Option<()> {
        for (&(s, e), &t) in nals.iter().zip(types) {
            if t == SLICE || t == IDR {
                let head = unescape(&unit[s..s + cut(&unit[s..e])]);
                let (_, fn_, _, poc) = self.fields(&head, t == IDR)?;
                if unit[s] & 0x60 != 0 {
                    self.frame_num = fn_;
                }
                if let (Some(sent), true) = (self.sent_poc, t != IDR) {
                    let step = ((poc as i64 - sent as i64).rem_euclid(self.mod_poc() as i64)) as u32;
                    self.poc_step = if 0 < step && step <= 16 { step } else { 2 };
                }
                self.sent_poc = Some(poc);
                self.poc = poc;
                return Some(());
            }
        }
        Some(())
    }

    fn fields(&self, head: &[u8], idr: bool) -> Option<Fields> {
        let sps = self.sps?;
        let mut b = Bits { data: head, pos: 8 };
        b.ue()?;
        b.ue()?;
        b.ue()?;
        let fn_pos = b.pos;
        let fn_ = b.u(sps.frame_num_bits)?;
        if !sps.frame_mbs_only && b.u(1)? == 1 {
            b.u(1)?;
        }
        if idr {
            b.ue()?;
        }
        if sps.poc_type != 0 {
            return Some((fn_pos, fn_, None, 0));
        }
        let poc_pos = b.pos;
        Some((fn_pos, fn_, Some(poc_pos), b.u(sps.poc_bits)?))
    }

    /// unit with frame_num and pic_order_cnt_lsb continuing from the last frame decoded.
    fn renumber(&mut self, unit: &[u8], nals: &[(usize, usize)], types: &[u8], copy: u32) -> Option<Vec<u8>> {
        self.decoded = true;
        if !self.rewriting {
            self.note(unit, nals, types);
            return Some(unit.to_vec());
        }
        let sps = self.sps?;
        let mut out = Vec::new();
        let mut numbers: Option<(u32, u32)> = None;
        for (&(s, e), &t) in nals.iter().zip(types) {
            let mut nal = unit[s..e].to_vec();
            if t == SLICE {
                let at = cut(&nal);
                let mut head = unescape(&nal[..at]);
                let (fn_pos, _, poc_pos, sent_poc) = self.fields(&head, false)?;
                if numbers.is_none() {
                    if let (0, Some(sent)) = (copy, self.sent_poc) {
                        let step = ((sent_poc as i64 - sent as i64).rem_euclid(self.mod_poc() as i64)) as u32;
                        if 0 < step && step <= 16 {
                            self.poc_step = step;
                        }
                        self.sent_poc = Some(sent_poc);
                    }
                    let frame_num = ((self.frame_num as u64 + 1) % (1u64 << sps.frame_num_bits)) as u32;
                    let poc = ((self.poc as u64 + self.poc_step as u64) % self.mod_poc()) as u32;
                    if nal[0] & 0x60 != 0 {
                        self.frame_num = frame_num;
                    }
                    self.poc = poc;
                    numbers = Some((frame_num, poc));
                }
                let (frame_num, poc) = numbers?;
                put(&mut head, fn_pos, sps.frame_num_bits, frame_num);
                if let Some(p) = poc_pos {
                    put(&mut head, p, sps.poc_bits, poc);
                }
                let mut changed = escape(&head);
                changed.extend_from_slice(&nal[at..]);
                nal = changed;
            }
            out.extend_from_slice(&START);
            out.extend_from_slice(&nal);
        }
        Some(out)
    }
}

impl PacketStep for Mosher {
    fn step(&mut self, unit: &[u8]) -> Vec<Vec<u8>> {
        // A unit that can't be read goes through as it came
        self.process(unit).unwrap_or_else(|| vec![unit.to_vec()])
    }
}

/// Makes a Mosher for each stream it's put on, all reading the same settings.
pub struct MosherFactory {
    pub settings: Arc<Mutex<Mosh>>,
    pub clean_up: bool,
}

impl PacketStepFactory for MosherFactory {
    fn make(&self) -> Box<dyn PacketStep> {
        Box::new(Mosher::new(self.settings.clone(), self.clean_up))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Arc<Mutex<Mosh>> {
        Arc::new(Mutex::new(Mosh::default()))
    }

    #[test]
    fn escaping_round_trips() {
        let raw = [0, 0, 1, 0, 0, 0, 0, 0, 3, 5, 0, 0, 2];
        assert_eq!(unescape(&escape(&raw)), raw);
        assert!(!escape(&raw).windows(3).any(|w| w == [0, 0, 1]));
    }

    #[test]
    fn nal_units_are_found_after_their_start_codes() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x65, 9, 0, 0];
        assert_eq!(nals(&data), vec![(4, 7), (10, 12)]);
    }

    #[test]
    fn exp_golomb_numbers_read_back() {
        // 1 = 0, 010 = 1, 011 = 2, 00100 = 3 and as signed: 010 = +1, 011 = -1
        let mut b = Bits { data: &[0b1010_0110, 0b0100_0000], pos: 0 };
        assert_eq!((b.ue(), b.ue(), b.ue(), b.ue()), (Some(0), Some(1), Some(2), Some(3)));
        let mut s = Bits { data: &[0b0100_1100], pos: 0 };
        assert_eq!((s.se(), s.se()), (Some(1), Some(-1)));
        let mut buf = [0u8; 2];
        put(&mut buf, 3, 7, 0b1010101);
        assert_eq!(Bits { data: &buf, pos: 3 }.u(7), Some(0b1010101));
    }

    #[test]
    fn a_baseline_sps_gives_the_header_layout() {
        // profile 66, level 30, id 0, frame_num 4 bits, POC type 0 with 6 bits, then the fields nothing here needs, frame_mbs_only
        let mut bits = vec![0x67, 66, 0xc0, 30];
        let mut w = Vec::new();
        for bit in "1 1 1 011 1 1 1 1 1".split(' ').flat_map(|s| s.chars()) {
            w.push((bit == '1') as u8);
        }
        while w.len() % 8 != 0 {
            w.push(0);
        }
        for byte in w.chunks(8) {
            bits.push(byte.iter().fold(0u8, |a, b| (a << 1) | b));
        }
        let sps = parse_sps(&bits).expect("an SPS");
        assert_eq!((sps.frame_num_bits, sps.poc_type, sps.poc_bits, sps.frame_mbs_only), (4, 0, 6, true));
    }

    #[test]
    fn units_with_no_slices_and_the_first_keyframe_go_through_as_they_came() {
        let mut m = Mosher::new(settings(), false);
        let aud = [0, 0, 0, 1, 0x09, 0xf0];
        assert_eq!(m.step(&aud), vec![aud.to_vec()]);
        // an IDR slice with no SPS seen yet
        let idr = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00];
        assert_eq!(m.step(&idr), vec![idr.to_vec()]);
        assert_eq!(MosherFactory { settings: settings(), clean_up: true }.make().step(&aud), vec![aud.to_vec()]);
    }
}
