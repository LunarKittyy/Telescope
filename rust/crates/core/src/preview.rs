//! What a plugin does with the preview's own copy of the picture: colour it, work something out of
//! it, and draw over it. Apps never see any of it, since none of it touches the frames that go out to the virtual camera.

use crate::frame::Frame;

/// What a pass's tint() does as plain data for the graphics card; whoever draws the preview moves `stripe_phase` on once per frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TintSpec {
    /// The brightness (0..255) zebras start at.
    pub zebra: Option<u8>,
    /// The Sobel gradients' mean, rounded, has to pass this to light up, and the colour it's drawn in.
    pub peaking: Option<(u8, [u8; 3])>,
    /// What each brightness (0..255) is coloured, R G B.
    pub false_colour: Option<&'static [[u8; 3]; 256]>,
    /// Where the zebras' stripes are, 0..8.
    pub stripe_phase: u8,
}

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

    /// What tint() does as data for the card, matching tint(), which stays as the preview's way without one; None when the card can't do this pass's tint.
    fn tint_spec(&self) -> Option<TintSpec> {
        None
    }

    /// The preview's copy of the frame as shown, already scaled to the size the preview paints it at (never above the stream's),
    /// as RGBA. Change it in place to colour the picture. Runs for each frame the preview shows, so keep it cheap.
    fn tint(&self, _frame: &mut Frame) {}

    /// How many times a second analyse() wants a picture; 0 for never. Read for each frame, so it can change.
    fn analyse_fps(&self) -> f32 {
        0.0
    }

    /// Whether analyse() only needs the copy shrunk to a small size (the scopes' width), so the host can hand it one read back small
    /// off the graphics card instead of a full-size one.
    fn analyses_small(&self) -> bool {
        false
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
