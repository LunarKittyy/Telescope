//! The capture time a phone stamps on each H.264 frame: an SEI NAL unit (user data unregistered) ahead of the picture, which rides through FFmpeg as
//! the packet's pts so a decoder that holds pictures back, or reorders them, still hands each one its own time. A newer phone puts the
//! crop its camera reported for the picture next to it, which goes with the same pts (CropTags).

use telescope_core::view::PhoneZoom;

/// "TELESCOPE-TIME11", which says an SEI is a capture time.
const UUID: &[u8; 16] = b"TELESCOPE-TIME11";

/// payload_type 5: user_data_unregistered.
const USER_DATA_UNREGISTERED: usize = 5;

/// UUID, the clock byte and the 8 byte time.
const PAYLOAD: usize = 16 + 1 + 8;

/// "TELESCOPE-CROP11", which says an SEI is the crop the camera reported for the picture.
const CROP_UUID: &[u8; 16] = b"TELESCOPE-CROP11";

/// UUID, then the zoom ratio, crop, x and y of the phone's `zoom` control as 4 byte floats.
const CROP_PAYLOAD: usize = 16 + 4 * 4;

/// How many pictures' crops are kept for a decoder that holds pictures back.
const CROPS_KEPT: usize = 32;

/// Bit of a pts that holds the clock byte, above any time a phone can have (2^62 ns is 146 years of uptime).
const CLOCK_BIT: u32 = 62;

/// When a phone's camera took a frame, on one of the phone's own clocks (core::clock::CLOCK_BOOT or CLOCK_MONO).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub clock: u8,
    pub ns: u64,
}

impl Stamp {
    /// As the pts of a packet, which FFmpeg hands back on the picture it decodes. None for a time or clock that doesn't fit.
    pub fn to_pts(self) -> Option<i64> {
        (self.clock < 2 && self.ns < 1 << CLOCK_BIT).then(|| (self.ns | u64::from(self.clock) << CLOCK_BIT) as i64)
    }

    /// The stamp a pts from to_pts holds; None for a pts that isn't one.
    pub fn from_pts(pts: i64) -> Option<Stamp> {
        let pts = u64::try_from(pts).ok()?;
        Some(Stamp { clock: (pts >> CLOCK_BIT) as u8, ns: pts & ((1 << CLOCK_BIT) - 1) })
    }
}

/// The capture time in an access unit (Annex-B bytes, as the parser splits them), None when it has none. With several, the last one counts.
#[cfg(test)]
pub fn find(unit: &[u8]) -> Option<Stamp> {
    read(unit).0
}

/// The capture time and the camera's crop in an access unit, each None when it has none. With several, the last one counts.
pub fn read(unit: &[u8]) -> (Option<Stamp>, Option<PhoneZoom>) {
    let mut found = (None, None);
    let starts = crate::h264::start_codes(unit);
    for (i, &(_, body)) in starts.iter().enumerate() {
        // An SEI unit is NAL type 6; the next start code, or the end of the unit, ends it
        if unit.get(body).is_none_or(|b| b & 31 != 6) {
            continue;
        }
        let end = starts.get(i + 1).map_or(unit.len(), |&(next, _)| next);
        let (stamp, crop) = in_sei(&unrescape(&unit[body + 1..end]));
        found = (stamp.or(found.0), crop.or(found.1));
    }
    found
}

/// A NAL unit's bytes with the emulation prevention taken out: the 03 after each 00 00 that only keeps the data from looking like a start code.
fn unrescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// The capture time and crop among the messages of an SEI's payload (after its NAL header, emulation prevention out).
fn in_sei(rbsp: &[u8]) -> (Option<Stamp>, Option<PhoneZoom>) {
    let (mut stamp, mut crop) = (None, None);
    let mut at = 0;
    // Messages follow each other until the trailing bits (0x80), which is no message
    while rbsp.len().saturating_sub(at) >= 2 && rbsp[at] != 0x80 {
        let Some((kind, after_kind)) = number(rbsp, at) else { break };
        let Some((size, start)) = number(rbsp, after_kind) else { break };
        let Some(payload) = start.checked_add(size).and_then(|end| rbsp.get(start..end)) else { break };
        if kind == USER_DATA_UNREGISTERED && size >= PAYLOAD && &payload[..16] == UUID {
            let time = u64::from_be_bytes(payload[17..PAYLOAD].try_into().unwrap_or_default());
            stamp = Some(Stamp { clock: payload[16], ns: time });
        }
        if kind == USER_DATA_UNREGISTERED && size >= CROP_PAYLOAD && &payload[..16] == CROP_UUID {
            crop = crop_of(&payload[16..CROP_PAYLOAD]);
        }
        at = start + size;
    }
    (stamp, crop)
}

/// The crop's four floats, None for values no phone sends (a ratio or crop under 1, a centre off the picture).
fn crop_of(bytes: &[u8]) -> Option<PhoneZoom> {
    let f = |i: usize| f64::from(f32::from_be_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap_or_default()));
    let (ratio, crop, x, y) = (f(0), f(1), f(2), f(3));
    let sane = [ratio, crop].iter().all(|v| (0.99..=1000.0).contains(v)) && [x, y].iter().all(|v| (0.0..=1.0).contains(v));
    sane.then(|| PhoneZoom { ratio: ratio.max(1.0), crop: crop.max(1.0), x, y })
}

/// The crops of the pictures given to a decoder lately, by the pts their capture time made, until the picture comes out.
#[derive(Default)]
pub struct CropTags(std::collections::VecDeque<(i64, PhoneZoom)>);

impl CropTags {
    pub fn note(&mut self, pts: Option<i64>, crop: Option<PhoneZoom>) {
        let (Some(pts), Some(crop)) = (pts, crop) else { return };
        if self.0.len() >= CROPS_KEPT {
            self.0.pop_front();
        }
        self.0.push_back((pts, crop));
    }

    /// The crop of the picture with this pts, forgetting it and any older one (a picture comes out once, and the ones before it are dropped or out already).
    pub fn take(&mut self, pts: Option<i64>) -> Option<PhoneZoom> {
        let i = self.0.iter().position(|(p, _)| Some(*p) == pts)?;
        let crop = self.0[i].1;
        self.0.drain(..=i);
        Some(crop)
    }
}

/// An SEI header number: 0xFF for each 255 it holds, then the rest. The number and where it ended.
fn number(bytes: &[u8], mut at: usize) -> Option<(usize, usize)> {
    let mut n = 0usize;
    loop {
        let b = *bytes.get(at)?;
        at += 1;
        n = n.checked_add(usize::from(b))?;
        if b != 0xFF {
            return Some((n, at));
        }
    }
}

/// An SEI NAL unit (with its 4 byte start code) holding a capture time, as the phone writes it: for tests of this app's own side.
#[cfg(test)]
pub(crate) fn sei(clock: u8, ns: u64) -> Vec<u8> {
    let mut body = vec![USER_DATA_UNREGISTERED as u8, PAYLOAD as u8];
    body.extend_from_slice(UUID);
    body.push(clock);
    body.extend_from_slice(&ns.to_be_bytes());
    body.push(0x80);
    // Emulation prevention: an 03 goes in after each 00 00 that a 00..03 follows
    let mut out = vec![0, 0, 0, 1, 6];
    let (mut zeros, mut nal) = (0, Vec::new());
    for &b in &body {
        if zeros >= 2 && b <= 3 {
            nal.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        nal.push(b);
    }
    out.extend_from_slice(&nal);
    out
}

/// An SEI NAL unit with a capture time and then the camera's crop, as a newer phone writes it: for tests of this app's own side.
#[cfg(test)]
pub(crate) fn sei_with_crop(clock: u8, ns: u64, crop: PhoneZoom) -> Vec<u8> {
    let time = sei(clock, ns);
    let mut body = vec![USER_DATA_UNREGISTERED as u8, CROP_PAYLOAD as u8];
    body.extend_from_slice(CROP_UUID);
    for v in [crop.ratio, crop.crop, crop.x, crop.y] {
        body.extend_from_slice(&(v as f32).to_be_bytes());
    }
    // Its own message in the same SEI, after the time's (whose trailing bits go)
    let mut out = time[..time.len() - 1].to_vec();
    let mut zeros = out.iter().rev().take_while(|&&b| b == 0).count();
    for &b in &body {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out.push(0x80);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x10];

    #[test]
    fn a_time_in_an_sei_in_front_of_a_picture_is_found() {
        let unit = [&[0, 0, 0, 1, 9, 0xF0][..], &sei(1, 812_345_678_901_234), VIDEO].concat();
        assert_eq!(find(&unit), Some(Stamp { clock: 1, ns: 812_345_678_901_234 }));
        assert_eq!(find(&[&sei(0, 5)[..], VIDEO].concat()), Some(Stamp { clock: 0, ns: 5 }));
    }

    #[test]
    fn a_value_that_needed_emulation_prevention_comes_back_whole() {
        // The bytes as the phone writes time 0x0000000300000000 on clock 1: 03 after each 00 00 that a 00..03 follows
        let wire = [0, 0, 0, 1, 6, 5, 25, b'T', b'E', b'L', b'E', b'S', b'C', b'O', b'P', b'E', b'-', b'T', b'I', b'M', b'E', b'1', b'1',
                    1, 0, 0, 3, 0, 3, 0, 0, 3, 0, 0, 0x80];
        let unit = [&wire[..], VIDEO].concat();
        assert_eq!(find(&unit), Some(Stamp { clock: 1, ns: 0x0000_0003_0000_0000 }));
        // And this side's writer says the same thing
        assert_eq!(sei(1, 0x0000_0003_0000_0000), wire);
    }

    #[test]
    fn times_that_are_all_zero_bytes_survive_too() {
        for ns in [0, 1, 0x0000_0000_0000_0300, 0x0003_0000_0000_0000, 0x0100_0000_0000_0000, u64::from(u32::MAX)] {
            assert_eq!(find(&[&sei(0, ns)[..], VIDEO].concat()), Some(Stamp { clock: 0, ns }), "{ns:#x}");
        }
    }

    #[test]
    fn no_sei_other_data_or_a_cut_off_one_gives_nothing() {
        assert_eq!(find(VIDEO), None);
        assert_eq!(find(&[]), None);
        // Another program's SEI (x264's own version message) is ignored
        let mut other = vec![0, 0, 0, 1, 6, 5, 25];
        other.extend_from_slice(b"x264 - core 164 r3095 ");
        other.extend_from_slice(&[1; 3]);
        other.push(0x80);
        assert_eq!(find(&[&other[..], VIDEO].concat()), None);
        // A time cut off before its end
        let whole = sei(0, 99);
        assert_eq!(find(&whole[..whole.len() - 6]), None);
        // An SEI with another payload type
        let mut recovery = vec![0, 0, 0, 1, 6, 6, 1, 0xC0, 0x80];
        recovery.extend_from_slice(VIDEO);
        assert_eq!(find(&recovery), None);
    }

    #[test]
    fn it_is_found_beside_other_messages_and_other_units() {
        // After a message of another kind in the same SEI, among SPS and PPS
        let mut sei_nal = vec![0, 0, 0, 1, 6, 1, 1, 0x40];
        let ours = sei(1, 4242);
        sei_nal.extend_from_slice(&ours[5..ours.len() - 1]);
        sei_nal.push(0x80);
        let unit = [&[0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E][..], &[0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80], &sei_nal, VIDEO].concat();
        assert_eq!(find(&unit), Some(Stamp { clock: 1, ns: 4242 }));
        // Three byte start codes
        let short = [&[0, 0, 1, 0x65, 1][..], &ours[1..], VIDEO].concat();
        assert_eq!(find(&short), Some(Stamp { clock: 1, ns: 4242 }));
    }

    #[test]
    fn a_stamp_goes_through_a_pts_and_back() {
        for stamp in [Stamp { clock: 0, ns: 0 }, Stamp { clock: 1, ns: 812_345_678_901_234 }, Stamp { clock: 0, ns: (1 << 62) - 1 }] {
            assert_eq!(Stamp::from_pts(stamp.to_pts().unwrap()), Some(stamp));
        }
        assert_eq!(Stamp { clock: 0, ns: 1 << 62 }.to_pts(), None);
        assert_eq!(Stamp { clock: 2, ns: 1 }.to_pts(), None);
        assert_eq!(Stamp::from_pts(-1), None);
    }

    #[test]
    fn a_crop_next_to_the_time_is_found_and_an_older_reader_still_finds_the_time() {
        let crop = PhoneZoom { ratio: 1.0, crop: 1.5, x: 0.25, y: 0.75 };
        let unit = [&[0, 0, 0, 1, 9, 0xF0][..], &sei_with_crop(1, 0x0000_0003_0000_0000, crop), VIDEO].concat();
        assert_eq!(read(&unit), (Some(Stamp { clock: 1, ns: 0x0000_0003_0000_0000 }), Some(crop)));
        assert_eq!(find(&unit), Some(Stamp { clock: 1, ns: 0x0000_0003_0000_0000 }));
        // The bytes the phone writes for time 0x0102030405060708 on clock 1 with crop 1, 2, 0.5, 0
        let mut wire = vec![0, 0, 0, 1, 6, 5, 25];
        wire.extend_from_slice(UUID);
        wire.extend_from_slice(&[1, 1, 2, 3, 4, 5, 6, 7, 8, 5, 32]);
        wire.extend_from_slice(CROP_UUID);
        wire.extend_from_slice(&[0x3F, 0x80, 0, 0, 0x40, 0, 0, 3, 0, 0x3F, 0, 0, 3, 0, 0, 3, 0, 0, 3, 0, 0x80]);
        let phone = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.0 };
        assert_eq!(read(&[&wire[..], VIDEO].concat()), (Some(Stamp { clock: 1, ns: 0x0102_0304_0506_0708 }), Some(phone)));
        assert_eq!(sei_with_crop(1, 0x0102_0304_0506_0708, phone), wire);
        // A phone that sends no crop
        assert_eq!(read(&[&sei(0, 5)[..], VIDEO].concat()), (Some(Stamp { clock: 0, ns: 5 }), None));
        // A crop no camera could have reported is no crop
        let odd = PhoneZoom { ratio: 1.0, crop: 0.5, x: 0.5, y: 0.5 };
        assert_eq!(read(&[&sei_with_crop(0, 5, odd)[..], VIDEO].concat()).1, None);
    }

    #[test]
    fn each_picture_gets_its_own_crop_however_late_it_comes_out() {
        let crop = |c: f64| PhoneZoom { ratio: 1.0, crop: c, x: 0.5, y: 0.5 };
        let mut tags = CropTags::default();
        for (pts, c) in [(10, 1.0), (20, 1.2), (30, 1.4)] {
            tags.note(Some(pts), Some(crop(c)));
        }
        tags.note(None, Some(crop(9.0)));
        tags.note(Some(40), None);
        assert_eq!(tags.take(Some(20)), Some(crop(1.2)));
        assert_eq!(tags.take(Some(10)), None, "an older picture was dropped");
        assert_eq!(tags.take(Some(40)), None, "a picture with no crop has none");
        assert_eq!(tags.take(Some(30)), Some(crop(1.4)));
        for pts in 0..100 {
            tags.note(Some(pts), Some(crop(1.0)));
        }
        assert_eq!(tags.0.len(), CROPS_KEPT);
    }
}
