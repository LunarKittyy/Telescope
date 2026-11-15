//! The capture time a phone stamps on each H.264 frame: an SEI NAL unit (user data unregistered) ahead of the picture, which rides through FFmpeg as
//! the packet's pts so a decoder that holds pictures back, or reorders them, still hands each one its own time.

/// "TELESCOPE-TIME11", which says an SEI is a capture time.
const UUID: &[u8; 16] = b"TELESCOPE-TIME11";

/// payload_type 5: user_data_unregistered.
const USER_DATA_UNREGISTERED: usize = 5;

/// UUID, the clock byte and the 8 byte time.
const PAYLOAD: usize = 16 + 1 + 8;

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
pub fn find(unit: &[u8]) -> Option<Stamp> {
    let mut found = None;
    let starts = crate::h264::start_codes(unit);
    for (i, &(_, body)) in starts.iter().enumerate() {
        // An SEI unit is NAL type 6; the next start code, or the end of the unit, ends it
        if unit.get(body).is_none_or(|b| b & 31 != 6) {
            continue;
        }
        let end = starts.get(i + 1).map_or(unit.len(), |&(next, _)| next);
        if let Some(stamp) = in_sei(&unrescape(&unit[body + 1..end])) {
            found = Some(stamp);
        }
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

/// The capture time among the messages of an SEI's payload (after its NAL header, emulation prevention out).
fn in_sei(rbsp: &[u8]) -> Option<Stamp> {
    let mut at = 0;
    // Messages follow each other until the trailing bits (0x80), which is no message
    while rbsp.len() - at >= 2 && rbsp[at] != 0x80 {
        let (kind, after_kind) = number(rbsp, at)?;
        let (size, start) = number(rbsp, after_kind)?;
        let payload = rbsp.get(start..start.checked_add(size)?)?;
        if kind == USER_DATA_UNREGISTERED && size >= PAYLOAD && &payload[..16] == UUID {
            let time = u64::from_be_bytes(payload[17..PAYLOAD].try_into().ok()?);
            return Some(Stamp { clock: payload[16], ns: time });
        }
        at = start + size;
    }
    None
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
}
