//! Where a phone's clocks stand against this computer's, so the capture time a phone stamps on each frame can be read on this computer's own clock.
//! The link crate measures it from round trips and the media crate reads it while decoding; they only share this.

use std::sync::Mutex;

/// The phone's `SystemClock.elapsedRealtimeNanos` clock (the clock byte of a capture time, and the `clock_boot_ns` of /v1/ping).
pub const CLOCK_BOOT: u8 = 0;
/// The phone's `System.nanoTime` clock (`clock_mono_ns` in /v1/ping).
pub const CLOCK_MONO: u8 = 1;

/// How far one phone's clocks are from this computer's monotonic clock (`monotonic_ns`), as measured over one round trip to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockOffset {
    /// The phone's boot clock minus this computer's clock at the same moment; None when the phone doesn't tell that clock.
    pub boot_ns: Option<i64>,
    /// The same for the phone's `System.nanoTime` clock.
    pub mono_ns: Option<i64>,
    /// How far off both can be: half the round trip they were measured over.
    pub error_ns: u64,
}

/// How well a stream's capture times line up with this computer's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSync {
    /// The most a capture time can be off by, in nanoseconds.
    pub error_ns: u64,
}

/// The offset between one phone's clocks and this computer's, as the latest measurement has it. The session that measures it sets it
/// and the decoder of that phone's video reads it, so a frame gets this computer's time as the picture is made.
#[derive(Debug, Default)]
pub struct PhoneClock {
    offset: Mutex<Option<ClockOffset>>,
}

impl PhoneClock {
    /// A new measurement, which replaces the one before.
    pub fn set(&self, offset: ClockOffset) {
        *self.offset.lock().unwrap_or_else(|e| e.into_inner()) = Some(offset);
    }

    /// The latest measurement, None until the first one.
    pub fn offset(&self) -> Option<ClockOffset> {
        *self.offset.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A time on one of the phone's clocks (CLOCK_BOOT or CLOCK_MONO) as this computer's monotonic nanoseconds, None until that clock's offset is known.
    pub fn to_desktop(&self, clock: u8, phone_ns: u64) -> Option<u64> {
        let o = self.offset()?;
        let offset = match clock {
            CLOCK_BOOT => o.boot_ns?,
            CLOCK_MONO => o.mono_ns?,
            _ => return None,
        };
        u64::try_from(i128::from(phone_ns) - i128::from(offset)).ok()
    }

    /// How good the offset is, None until it's known.
    pub fn sync(&self) -> Option<ClockSync> {
        self.offset().map(|o| ClockSync { error_ns: o.error_ns })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_time_becomes_this_computers_by_the_offset_of_its_clock() {
        let clock = PhoneClock::default();
        assert!(clock.to_desktop(CLOCK_BOOT, 5).is_none() && clock.sync().is_none());
        clock.set(ClockOffset { boot_ns: Some(1_000), mono_ns: Some(-500), error_ns: 7 });
        assert_eq!(clock.to_desktop(CLOCK_BOOT, 4_000), Some(3_000));
        assert_eq!(clock.to_desktop(CLOCK_MONO, 4_000), Some(4_500));
        assert_eq!(clock.sync(), Some(ClockSync { error_ns: 7 }));
    }

    #[test]
    fn a_clock_the_phone_didnt_tell_or_a_time_before_zero_gives_nothing() {
        let clock = PhoneClock::default();
        clock.set(ClockOffset { boot_ns: Some(1_000), mono_ns: None, error_ns: 0 });
        assert_eq!(clock.to_desktop(CLOCK_MONO, 4_000), None);
        assert_eq!(clock.to_desktop(9, 4_000), None);
        assert_eq!(clock.to_desktop(CLOCK_BOOT, 999), None);
    }
}
