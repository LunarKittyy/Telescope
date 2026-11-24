//! A decoder's picture left on the card for the virtual camera, which asks for it scaled to its own size so only that much comes down.

use std::sync::{Arc, Mutex};
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
/// The scaled one is kept, so the camera and the preview that paint from it share one scaling.
pub(crate) struct SurfacePicture {
    surface: Arc<dyn DecodedSurface>,
    fitted: Mutex<Option<((u32, u32), Frame)>>,
}

impl SurfacePicture {
    pub(crate) fn new(surface: Arc<dyn DecodedSurface>) -> Self {
        Self { surface, fitted: Mutex::new(None) }
    }

    /// What was scaled for this size already. Never waits: a scaling holds the lock across the card's readback, and the window's thread asks this too.
    fn kept(&self, width: u32, height: u32) -> Option<Frame> {
        let kept = match self.fitted.try_lock() {
            Ok(kept) => kept,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        kept.as_ref().filter(|(size, _)| *size == (width, height)).map(|(_, frame)| frame.clone())
    }
}

impl CardPicture for SurfacePicture {
    fn land(&self) -> Frame {
        self.surface.ram().clone()
    }

    fn landed(&self) -> Option<Frame> {
        self.surface.landed().cloned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn fits_on_card(&self, width: u32, height: u32) -> bool {
        self.surface.can_fit(width, height)
    }

    fn keeps_fitted(&self) -> bool {
        true
    }

    fn landed_fitted(&self, width: u32, height: u32) -> Option<Frame> {
        self.kept(width, height)
    }

    fn land_fitted(&self, width: u32, height: u32, _cancel: &dyn Fn() -> bool) -> Option<Frame> {
        // One in memory already is fitted from there; a second readback would cost more than it saves
        if self.surface.landed().is_some() || !self.surface.can_fit(width, height) {
            return None;
        }
        // Held across the scaling, so two readers asking at once scale the picture once
        let mut kept = self.fitted.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((size, frame)) = kept.as_ref() {
            if *size == (width, height) {
                return Some(frame.clone());
            }
        }
        let frame = self.surface.fitted(width, height)?;
        *kept = Some(((width, height), frame.clone()));
        Some(frame)
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

    #[derive(Debug)]
    struct Whole(Frame);
    impl DecodedSurface for Whole {
        fn ram(&self) -> &Frame {
            &self.0
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Scales to a grey picture, but only once the test lets it (a card backed up with readbacks).
    #[derive(Debug)]
    struct Backed(Frame, std::sync::Mutex<std::sync::mpsc::Receiver<()>>, std::sync::mpsc::Sender<()>);
    impl DecodedSurface for Backed {
        fn ram(&self) -> &Frame {
            &self.0
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn can_fit(&self, _width: u32, _height: u32) -> bool {
            true
        }
        fn fitted(&self, width: u32, height: u32) -> Option<Frame> {
            self.2.send(()).unwrap();
            let _ = self.1.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10));
            Some(Frame::dark(width, height, telescope_core::frame::PixelFormat::I420))
        }
    }

    #[test]
    fn asking_for_the_scaled_copy_never_waits_for_a_scaling_that_is_running() {
        let (release, released) = std::sync::mpsc::channel();
        let (started, running) = std::sync::mpsc::channel();
        let picture = Arc::new(SurfacePicture::new(Arc::new(Backed(Frame::dark(8, 8, telescope_core::frame::PixelFormat::I420), std::sync::Mutex::new(released), started))));
        let scaling = std::thread::spawn({
            let picture = picture.clone();
            move || picture.land_fitted(4, 4, &|| false)
        });
        running.recv_timeout(std::time::Duration::from_secs(5)).expect("the scaling began");
        // The window's thread asks while the scaling holds the lock: the answer is "not yet", at once
        let (asked, answer) = std::sync::mpsc::channel();
        let reader = std::thread::spawn({
            let picture = picture.clone();
            move || asked.send(picture.landed_fitted(4, 4).is_none()).unwrap()
        });
        let waited = answer.recv_timeout(std::time::Duration::from_secs(2));
        release.send(()).unwrap();
        assert_eq!(waited, Ok(true), "landed_fitted waited for the card");
        reader.join().unwrap();
        assert!(scaling.join().unwrap().is_some());
        assert!(picture.landed_fitted(4, 4).is_some(), "the scaled copy isn't kept once it is made");
    }

    #[test]
    fn only_a_surface_picture_keeps_what_it_fitted() {
        let frame = Frame::dark(4, 4, telescope_core::frame::PixelFormat::I420);
        assert!(SurfacePicture::new(Arc::new(Whole(frame))).keeps_fitted());
    }

    #[test]
    fn the_camera_size_survives_the_trip_through_one_number() {
        assert_eq!(unpack_camera_fit(pack_camera_fit((1920, 1080))), Some((1920, 1080)));
        assert_eq!(unpack_camera_fit(0), None);
    }
}
