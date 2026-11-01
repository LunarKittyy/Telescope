//! Pictures as they travel from the decoder through the plugins to the virtual camera, and the steps that change them.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Weak};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PixelFormat {
    /// Y plane, then U, then V at half size: what the decoder makes and v4l2loopback takes.
    I420,
    /// Y plane, then interleaved UV: what the Windows camera's shared memory takes.
    Nv12,
    /// 8-bit B, G, R, A per pixel.
    Bgra,
    /// 8-bit R, G, B, A per pixel.
    Rgba,
}

impl PixelFormat {
    /// Bytes for a packed picture of this size (no row padding; chroma rounded up for odd sizes).
    pub fn size(self, width: u32, height: u32) -> usize {
        let (w, h) = (width as usize, height as usize);
        match self {
            PixelFormat::I420 | PixelFormat::Nv12 => w * h + 2 * w.div_ceil(2) * h.div_ceil(2),
            PixelFormat::Bgra | PixelFormat::Rgba => w * h * 4,
        }
    }
}

/// One picture. Cloning shares the pixels; a step that changes them gets its own copy (data_mut), so readers never see a half-changed frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Packed planes, no row padding.
    pub data: Arc<Vec<u8>>,
    /// Goes up with every frame any stream makes, so a different number is a different frame.
    pub number: u64,
    /// When the stream made it, monotonic nanoseconds (crate::time::monotonic_ns).
    pub time_ns: u64,
    decoded: Option<Decoded>,
}

/// A picture as 8-bit B, G, R, rows `stride` bytes apart from `start`: what a decoder's colour conversion made.
#[derive(Debug, Default, PartialEq)]
pub struct Bgr {
    pub data: Vec<u8>,
    pub start: usize,
    pub stride: usize,
}

impl Bgr {
    pub fn row(&self, y: usize, width: usize) -> &[u8] {
        &self.data[self.start + y * self.stride..][..width * 3]
    }
}

/// The BGR a frame's pixels were made from. `of` names those pixels, so anything that gives the frame others leaves this behind.
#[derive(Clone, Debug)]
struct Decoded {
    of: Weak<Vec<u8>>,
    picture: Arc<Bgr>,
}

/// The same picture as the frame's pixels, so it never decides whether two frames are equal.
impl PartialEq for Decoded {
    fn eq(&self, _: &Decoded) -> bool {
        true
    }
}

impl Frame {
    pub fn new(width: u32, height: u32, format: PixelFormat, data: Vec<u8>) -> Frame {
        debug_assert_eq!(data.len(), format.size(width, height));
        Frame { width, height, format, data: Arc::new(data), number: 0, time_ns: crate::time::monotonic_ns(), decoded: None }
    }

    /// The pixels to change, copied first if anything else still holds them.
    pub fn data_mut(&mut self) -> &mut Vec<u8> {
        self.decoded = None;
        Arc::make_mut(&mut self.data)
    }

    /// The frame with the BGR its pixels were converted from, which effects read in place of the lossier pixels.
    pub fn with_bgr(mut self, picture: Arc<Bgr>) -> Frame {
        self.decoded = Some(Decoded { of: Arc::downgrade(&self.data), picture });
        self
    }

    /// The BGR this picture was made from, while the pixels are still that picture.
    pub fn bgr(&self) -> Option<&Bgr> {
        self.decoded.as_ref().filter(|d| Weak::as_ptr(&d.of) == Arc::as_ptr(&self.data)).map(|d| &*d.picture)
    }

    /// The I420 planes (Y, U, V); None for other formats.
    pub fn i420_planes(&self) -> Option<(&[u8], &[u8], &[u8])> {
        if self.format != PixelFormat::I420 {
            return None;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let c = w.div_ceil(2) * h.div_ceil(2);
        let (y, rest) = self.data.split_at(w * h);
        let (u, v) = rest.split_at(c);
        Some((y, u, v))
    }
}

/// The newest picture of one running stream, as it goes out to its virtual camera (after that stream's plugins).
#[derive(Clone, Debug)]
pub struct StreamFrame {
    /// A phone's id or a StreamSource's ("" for the stream with no phone picked).
    pub source_id: String,
    pub name: String,
    pub frame: Frame,
}

impl StreamFrame {
    /// How long ago (in ns) the frame was published, at now; a dropped stream's last frame only gets older.
    pub fn age(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.frame.time_ns)
    }
}

/// Where a plugin's frame step runs (FRAME_STAGES); frames go through the stages in this order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameStage {
    /// The window's zoom, pan, flip, rotation and stretch.
    View,
    /// Moving the whole picture after the view (Desk mode).
    Geometry,
    /// Looking at the clean picture without changing it (QR codes).
    Reading,
    /// What's behind you (Background, Chroma key).
    Background,
    /// Changing the picture (Fun pack), and where a plugin goes that doesn't say or names one that doesn't exist.
    #[default]
    Effects,
    /// The colour pipeline: every grade baked into one LUT per stream.
    Colour,
    /// Hiding things, after anything that could bring them back (Privacy blur).
    Privacy,
    /// Drawn on top, never graded or blurred (Name tag).
    Overlay,
    /// Several streams put into one picture.
    Combine,
    /// The wait screen in place of the picture.
    Cover,
    /// Seeing what apps get and handing it back unchanged (Preview).
    Final,
}

impl FrameStage {
    pub const ALL: [FrameStage; 11] = [
        FrameStage::View, FrameStage::Geometry, FrameStage::Reading, FrameStage::Background, FrameStage::Effects,
        FrameStage::Colour, FrameStage::Privacy, FrameStage::Overlay, FrameStage::Combine, FrameStage::Cover, FrameStage::Final,
    ];
}

/// What a frame step may ask while it works on a frame (HostFrames.frame_source, HostVision.person_mask, HostColour.colour_preview).
pub trait FrameContext {
    /// The source id of the stream this frame is from.
    fn source_id(&self) -> &str;
    /// How sure the vision service is each pixel is a person (0..255, the frame's size), or None while it can't say.
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>>;
    /// For a final-stage step: the frame as the preview shows it, with a colour_split drawn in.
    fn colour_preview(&self, frame: &Frame) -> Frame;
}

/// What a CpuStep is about to do with a frame: the less it does, the longer the picture can stay on the graphics card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    /// Skips this frame; process() isn't called, and pixels on the card stay there.
    Nothing,
    /// Looks at the pixels and leaves them as they are; they come down, but the card keeps its copy for the next shader.
    Reads,
    /// Changes the pixels, or says nothing: the card's copy is dropped and a shader after it has to upload the frame again.
    Changes,
}

/// A frame step in Rust, run on the stream's thread.
pub trait CpuStep: Send {
    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext);
    /// Asked right before process. Only a step that's certain may answer less than Changes.
    fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
        Touch::Changes
    }
}

/// A frame step as a GPU shader, the only kind a Lua or WASM plugin hands over: plain data the host compiles and runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShaderStep {
    /// WGSL source with the entry points the plugin host documents.
    pub wgsl: String,
    /// Values for the shader's uniforms, by name.
    pub uniforms: Value,
    /// Extra pictures it samples (a LUT, a logo), by binding name.
    #[serde(default)]
    pub textures: Vec<(String, Frame)>,
}

impl Serialize for Frame {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Frame", 4)?;
        st.serialize_field("width", &self.width)?;
        st.serialize_field("height", &self.height)?;
        st.serialize_field("format", &self.format)?;
        st.serialize_field("data", &*self.data)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for Frame {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Frame, D::Error> {
        #[derive(Deserialize)]
        struct Plain {
            width: u32,
            height: u32,
            format: PixelFormat,
            data: Vec<u8>,
        }
        let p = Plain::deserialize(d)?;
        if p.data.len() != p.format.size(p.width, p.height) {
            return Err(serde::de::Error::custom("frame data doesn't match its size"));
        }
        Ok(Frame::new(p.width, p.height, p.format, p.data))
    }
}

/// A plugin's process_frame frozen at its current settings; the plugin hands out a new one when they change.
pub enum FrameStep {
    Cpu(Box<dyn CpuStep>),
    Shader(ShaderStep),
}

impl std::fmt::Debug for FrameStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameStep::Cpu(_) => f.write_str("FrameStep::Cpu"),
            FrameStep::Shader(s) => f.debug_tuple("FrameStep::Shader").field(&s.wgsl.len()).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_a_shared_frame_copies_it() {
        let a = Frame::new(2, 2, PixelFormat::I420, vec![1; 6]);
        let mut b = a.clone();
        b.data_mut()[0] = 9;
        assert_eq!(a.data[0], 1);
        assert_eq!(b.data[0], 9);
        assert_eq!(b.i420_planes().unwrap().1, &[1]);
    }

    #[test]
    fn the_decoders_bgr_stays_only_while_the_pixels_are_its_picture() {
        let picture = Arc::new(Bgr { data: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13], start: 1, stride: 6 });
        let frame = Frame::new(2, 2, PixelFormat::I420, vec![1; 6]).with_bgr(picture.clone());
        assert_eq!(frame.bgr().unwrap().row(1, 2), [8, 9, 10, 11, 12, 13]);
        let shared = frame.clone();
        assert_eq!(shared.bgr(), Some(&*picture), "a clone is the same picture");

        let mut changed = frame.clone();
        changed.data_mut()[0] = 9;
        assert_eq!(changed.bgr(), None);
        assert_eq!(frame.bgr(), Some(&*picture), "the other holders still see the old pixels");

        let mut replaced = frame.clone();
        replaced.data = Arc::new(vec![2; 6]);
        assert_eq!(replaced.bgr(), None, "even if it went round data_mut");
        let mut alone = Frame::new(2, 2, PixelFormat::I420, vec![1; 6]).with_bgr(picture.clone());
        Arc::make_mut(&mut alone.data)[0] = 9;
        assert_eq!(alone.bgr(), None, "the only holder, changed in place");
    }

    #[test]
    fn stages_sort_in_frame_order() {
        let mut s = FrameStage::ALL.to_vec();
        s.reverse();
        s.sort();
        assert_eq!(s, FrameStage::ALL);
        assert_eq!(serde_json::to_value(FrameStage::Colour).unwrap(), "colour");
    }
}
