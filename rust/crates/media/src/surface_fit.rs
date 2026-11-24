//! A decoder's picture left on the card for the virtual camera, which asks for it scaled to its own size so only that much comes down.

use std::sync::Arc;
use telescope_core::frame::{DecodedSurface, Frame};
use telescope_core::image::fit_box;
use telescope_core::media::CardPicture;

/// Whether the card can scale a picture of `source` size to exactly the camera's size: smaller in both directions, even, and the same shape (no bars to draw).
pub(crate) fn scales_to(source: (u32, u32), camera: (u32, u32)) -> bool {
    let (w, h) = camera;
    w > 0 && h > 0 && w % 2 == 0 && h % 2 == 0 && w < source.0 && h < source.1 && fit_box(source.0, source.1, w, h) == (0, 0, w, h)
}

/// Whether a plain stream's pictures stay on the card for the camera: only on Windows, where the card's video processor does the scaling, and while no step
/// needs the picture, the camera is open (a size is given) and takes a smaller one than the stream's.
pub(crate) fn wanted(windows: bool, plain: bool, camera: Option<(u32, u32)>, source: (u32, u32)) -> bool {
    windows && plain && camera.is_some_and(|c| scales_to(source, c))
}

/// A card picture that is still the decoder's surface: it lands whole as it always did, or scaled to the camera's size when the card can.
pub(crate) struct SurfacePicture(pub(crate) Arc<dyn DecodedSurface>);

impl CardPicture for SurfacePicture {
    fn land(&self) -> Frame {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
            log::info!("DIAG a kept picture landed whole (backtrace {})", std::backtrace::Backtrace::force_capture().to_string().lines().filter(|l| l.contains("telescope")).take(6).collect::<Vec<_>>().join(" | "));
        }
        self.0.ram().clone()
    }

    fn landed(&self) -> Option<Frame> {
        self.0.landed().cloned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn fits_on_card(&self, width: u32, height: u32) -> bool {
        self.0.can_fit(width, height)
    }

    fn land_fitted(&self, width: u32, height: u32, _cancel: &dyn Fn() -> bool) -> Option<Frame> {
        // One in memory already is fitted from there; a second readback would cost more than it saves
        if self.0.landed().is_some() || !self.0.can_fit(width, height) {
            return None;
        }
        self.0.fitted(width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::media::{pack_camera_fit, unpack_camera_fit};

    #[test]
    fn only_a_smaller_camera_of_the_same_shape_is_scaled_to() {
        assert!(scales_to((3840, 2160), (1920, 1080)));
        assert!(scales_to((3840, 2160), (1280, 720)));
        assert!(!scales_to((1920, 1080), (1920, 1080)), "same size");
        assert!(!scales_to((1920, 1080), (3840, 2160)), "a bigger camera");
        assert!(!scales_to((3840, 2160), (1920, 1440)), "another shape would need bars");
        assert!(!scales_to((3840, 2160), (1919, 1079)), "odd sizes can't be NV12");
        assert!(!scales_to((3840, 2160), (0, 0)));
        assert!(!scales_to((3840, 2160), (3840, 1080)), "narrower in one direction only");
    }

    #[test]
    fn a_stream_is_scaled_only_when_plain_on_windows_with_a_smaller_camera_open() {
        let big = (3840, 2160);
        assert!(wanted(true, true, Some((1920, 1080)), big));
        assert!(!wanted(false, true, Some((1920, 1080)), big), "Linux fits on the card another way");
        assert!(!wanted(true, false, Some((1920, 1080)), big), "a step needs the whole picture");
        assert!(!wanted(true, true, None, big), "no camera open");
        assert!(!wanted(true, true, Some(big), big), "camera as big as the stream");
    }

    #[test]
    fn the_camera_size_survives_the_trip_through_one_number() {
        assert_eq!(unpack_camera_fit(pack_camera_fit((1920, 1080))), Some((1920, 1080)));
        assert_eq!(unpack_camera_fit(0), None);
    }
}
