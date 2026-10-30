//! What a plugin does with the preview's own copy of the picture (PreviewLayer in overlays.py): colour it, work something out of
//! it, and draw over it. Apps never see any of it, since none of it touches the frames that go out to the virtual camera.

use crate::frame::Frame;

/// One plugin's work on the preview, put there with HostPreview::add_preview_pass. The host calls it from its own threads, so
/// whatever it keeps sits behind its own locks.
pub trait PreviewPass: Send + Sync {
    /// Passes with a lower order are tinted and painted first, under the others.
    fn order(&self) -> i32 {
        0
    }

    /// Whether tint() changes anything right now. The host reads the pictures it tints off the graphics card only while some
    /// pass says yes.
    fn tints(&self) -> bool {
        false
    }

    /// The preview's copy of the frame as shown, already scaled to the size the preview paints it at (never above the stream's),
    /// as RGBA. Change it in place to colour the picture. Runs for each frame the preview shows, so keep it cheap.
    fn tint(&self, _frame: &mut Frame) {}

    /// How many times a second analyse() wants a picture; 0 for never. Read for each frame, so it can change.
    fn analyse_fps(&self) -> f32 {
        0.0
    }

    /// On a thread of its own, at most analyse_fps a second: a copy of the preview's frame before any tint, to work something
    /// out from (a scope). True asks for paint() to run again.
    fn analyse(&self, _frame: &Frame) -> bool {
        false
    }

    /// Drawn over the frame, which fills width x height pixels of the preview (scale is the screen's pixels to the logical ones
    /// the layout uses). RGBA with straight alpha; None draws nothing. Asked for again after HostPreview::refresh_preview or
    /// when analyse() says so.
    fn paint(&self, _width: u32, _height: u32, _scale: f32) -> Option<Frame> {
        None
    }
}
