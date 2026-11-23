//! Sending a smaller picture than the one picked while frames run late: the camera never gets more than its own size, so a bigger
//! stream is only extra work for the decoder. Which size is still enough is worked out here; Stream output decides when to use it.

/// How long after Telescope changed the size itself a late reading counts for nothing: the phone is reopening its camera.
pub const SETTLE_NS: u64 = 15_000_000_000;
/// A crop on this computer above this much wants the whole picture (zoom 1 with a hair of rounding).
pub const CROP_ABOVE: f64 = 1.02;

/// The size picked while a smaller one is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trim {
    pub picked: (u32, u32),
}

/// The smallest of `listed` (the sizes with the picture's shape) that still gives a camera of `fit` all the pixels it can use from a
/// `from` picture, and isn't bigger than `from`. None when nothing smaller does.
pub fn smallest_enough(listed: &[(u32, u32)], from: (u32, u32), fit: (u32, u32)) -> Option<(u32, u32)> {
    let scale = (fit.0 as f64 / from.0 as f64).min(fit.1 as f64 / from.1 as f64).min(1.0);
    let (need_w, need_h) = (from.0 as f64 * scale, from.1 as f64 * scale);
    let area = |s: &(u32, u32)| s.0 as u64 * s.1 as u64;
    listed.iter().copied()
        .filter(|s| s.0 as f64 >= need_w - 0.5 && s.1 as f64 >= need_h - 0.5 && area(s) < area(&from))
        .min_by_key(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [(u32, u32); 4] = [(3840, 2160), (1920, 1080), (1280, 720), (640, 360)];

    #[test]
    fn four_k_into_a_1080p_camera_needs_1080p() {
        assert_eq!(smallest_enough(&SIZES, (3840, 2160), (1920, 1080)), Some((1920, 1080)));
    }

    #[test]
    fn a_camera_between_two_sizes_gets_the_bigger() {
        assert_eq!(smallest_enough(&SIZES, (3840, 2160), (1600, 900)), Some((1920, 1080)));
    }

    #[test]
    fn a_camera_as_big_as_the_picture_or_bigger_trims_nothing() {
        assert_eq!(smallest_enough(&SIZES, (1920, 1080), (1920, 1080)), None);
        assert_eq!(smallest_enough(&SIZES, (1920, 1080), (3840, 2160)), None);
    }

    #[test]
    fn a_camera_of_another_shape_is_filled_on_its_short_side() {
        // A 4:3 camera takes the 16:9 picture with bars: the picture is as wide as the camera, so 1280 wide is plenty
        assert_eq!(smallest_enough(&SIZES, (3840, 2160), (1280, 960)), Some((1280, 720)));
        // A tall camera: the picture is as wide as the camera, and 720 wide is more than 640
        assert_eq!(smallest_enough(&SIZES, (3840, 2160), (720, 1280)), Some((1280, 720)));
    }

    #[test]
    fn nothing_listed_is_enough_when_the_camera_wants_a_size_between_none() {
        assert_eq!(smallest_enough(&[(3840, 2160)], (3840, 2160), (1920, 1080)), None);
    }
}
