//! Pictures as they travel from the decoder through the plugins to the virtual camera, and the steps that change them.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock, Weak};

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
    /// Packed planes, no row padding; a decoder surface has metadata here until materialise is called.
    pub data: Arc<Vec<u8>>,
    /// Goes up with every frame any stream makes, so a different number is a different frame.
    pub number: u64,
    /// When the stream made it, monotonic nanoseconds (crate::time::monotonic_ns).
    pub time_ns: u64,
    /// When the camera took the picture, on this computer's monotonic clock (crate::time::monotonic_ns), for a stream whose phone says so and whose
    /// clock offset is known; None otherwise. Every step that makes a new frame from another keeps it.
    pub capture_ns: Option<u64>,
    /// The share of its view the phone's camera says this picture shows (the crop tag in the stream), for a phone that says so; None otherwise. Copies made before the view's crop keep it.
    pub phone_share: Option<crate::view::PhoneZoom>,
    /// The view in full source coordinates, for remapping a held person mask; None is the unchanged source.
    pub geometry: Option<Box<crate::view::Geometry>>,
    /// Source field of view and any geometry that cannot be remapped in camera coordinates.
    pub mask_space: MaskSpace,
    decoded: Option<Decoded>,
    /// How much of each pixel is the picture, for a frame whose background was cut out; None for a frame that is all picture, which is every frame today.
    alpha: Option<Arc<Alpha>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaskSpace {
    pub source_size: (u32, u32),
    pub source_epoch: u64,
    pub warp: Option<u64>,
    /// The crop in `Frame::geometry` is an estimate that can be well off: the phone's zoom is still arriving and its frames don't say how far it has got. Held masks aren't moved by it then.
    pub crop_unsure: bool,
}

/// One 8-bit plane of how much of each pixel is the picture (255) and how much is whatever goes behind it (0), at the frame's full size.
#[derive(Clone, Debug, PartialEq)]
pub struct Alpha {
    pub w: u32,
    pub h: u32,
    /// w * h bytes, one a pixel, rows packed.
    pub data: Vec<u8>,
    /// Whether the frame's colour planes are already multiplied by this (each plane value times alpha, chroma included), or straight.
    pub premultiplied: bool,
    /// Opaque outputs keep the rendered backdrop when no foreground was available.
    keep_opaque: bool,
}

impl Alpha {
    pub fn new(w: u32, h: u32, data: Vec<u8>, premultiplied: bool) -> Alpha {
        debug_assert_eq!(data.len(), w as usize * h as usize);
        Alpha { w, h, data, premultiplied, keep_opaque: false }
    }

    /// Keep the rendered picture on opaque outputs if this cutout has no foreground coverage.
    pub fn keep_opaque_when_empty(mut self) -> Alpha {
        self.keep_opaque = !self.premultiplied && self.data.iter().all(|&v| v == 0);
        self
    }
}

/// x / 255 rounded, for x up to 255 * 255.
#[inline(always)]
fn div255(x: u32) -> u32 {
    (x + 127) / 255
}

/// One colour value over a background value, by how much of it is the picture.
#[inline(always)]
fn over(colour: u8, a: u8, behind: u8, premultiplied: bool) -> u8 {
    let (a, behind, colour) = (a as u32, behind as u32, colour as u32);
    let under = behind * (255 - a);
    if premultiplied { (colour + div255(under)).min(255) as u8 } else { div255(colour * a + under) as u8 }
}

/// An R, G, B colour as video-range BT.601 Y, U, V, which is what the I420 and NV12 planes hold.
fn yuv_of([r, g, b]: [u8; 3]) -> [u8; 3] {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    let byte = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u8;
    [byte(16.0 + (65.481 * r + 128.553 * g + 24.966 * b) / 255.0),
     byte(128.0 + (-37.797 * r - 74.203 * g + 112.0 * b) / 255.0),
     byte(128.0 + (112.0 * r - 93.786 * g - 18.214 * b) / 255.0)]
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

/// Buffers of BGR pictures that are no longer in use, for the next one to be written over (a 4K picture is 24 MB to fault in otherwise).
static BGR_SPARES: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// How many spare BGR buffers are kept.
const KEPT_BGR: usize = 1;

/// A buffer for a BGR picture: one that was given back, or an empty one.
pub fn spare_bgr() -> Vec<u8> {
    BGR_SPARES.lock().map(|mut s| s.pop()).ok().flatten().unwrap_or_default()
}

/// A picture's BGR that is made the first time something asks for it, once, however many frames share it.
pub struct LazyBgr {
    picture: OnceLock<Arc<Bgr>>,
    make: Mutex<Option<Box<dyn FnOnce() -> Bgr + Send>>>,
}

impl LazyBgr {
    /// A picture that is made by `make` when it is first read.
    pub fn new(make: impl FnOnce() -> Bgr + Send + 'static) -> LazyBgr {
        LazyBgr { picture: OnceLock::new(), make: Mutex::new(Some(Box::new(make))) }
    }

    /// A picture that is already made.
    pub fn ready(picture: Arc<Bgr>) -> LazyBgr {
        LazyBgr { picture: OnceLock::from(picture), make: Mutex::new(None) }
    }

    /// The picture, made now if nothing has asked yet.
    pub fn get(&self) -> &Bgr {
        self.picture.get_or_init(|| {
            let make = self.make.lock().ok().and_then(|mut m| m.take());
            Arc::new(make.map(|m| m()).unwrap_or_default())
        })
    }

    /// Whether something has asked for the picture yet.
    pub fn is_made(&self) -> bool {
        self.picture.get().is_some()
    }
}

impl std::fmt::Debug for LazyBgr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyBgr").field("made", &self.is_made()).finish()
    }
}

impl Drop for LazyBgr {
    fn drop(&mut self) {
        // Its buffer is written over by the next picture, unless a reader still holds it
        let Some(Ok(bgr)) = self.picture.take().map(Arc::try_unwrap) else { return };
        if let Ok(mut spares) = BGR_SPARES.lock() {
            if spares.len() < KEPT_BGR {
                spares.push(bgr.data);
            }
        }
    }
}

/// The BGR a frame's pixels were made from. `of` names those pixels, so anything that gives the frame others leaves this behind.
#[derive(Clone, Debug)]
struct Decoded {
    of: Weak<Vec<u8>>,
    picture: Option<Arc<LazyBgr>>,
    surface: Option<Arc<dyn DecodedSurface>>,
    size: (u32, u32, PixelFormat),
}

/// A decoder-owned surface; media supplies its import and its cached RAM copy.
pub trait DecodedSurface: std::fmt::Debug + Send + Sync {
    fn ram(&self) -> &Frame;
    fn as_any(&self) -> &dyn std::any::Any;
}

/// The same picture as the frame's pixels, so it never decides whether two frames are equal.
impl PartialEq for Decoded {
    fn eq(&self, _: &Decoded) -> bool {
        true
    }
}

impl Frame {
    /// Metadata for a decoder surface. Call materialise before reading the public data field.
    pub fn from_surface(width: u32, height: u32, format: PixelFormat, surface: Arc<dyn DecodedSurface>) -> Frame {
        let data = Arc::new(Vec::new());
        let decoded = Some(Decoded { of: Arc::downgrade(&data), picture: None, surface: Some(surface), size: (width, height, format) });
        Frame { width, height, format, data, number: 0, time_ns: crate::time::monotonic_ns(), capture_ns: None, phone_share: None, geometry: None, mask_space: MaskSpace::default(), decoded, alpha: None }
    }

    /// A flat dark picture: black luma and neutral chroma for the YUV formats, opaque black for the others.
    pub fn dark(width: u32, height: u32, format: PixelFormat) -> Frame {
        let (luma, all) = (width as usize * height as usize, format.size(width, height));
        let data = match format {
            PixelFormat::I420 | PixelFormat::Nv12 => [vec![16; luma], vec![128; all - luma]].concat(),
            _ => [0, 0, 0, 255].repeat(luma),
        };
        Frame::new(width, height, format, data)
    }

    /// The frame's size, format and stamps with no pixels and no hold on a decoder surface, for a record of a picture that is kept elsewhere.
    pub fn without_pixels(&self) -> Frame {
        Frame { width: self.width, height: self.height, format: self.format, data: Arc::default(), number: self.number, time_ns: self.time_ns, capture_ns: self.capture_ns, phone_share: self.phone_share, geometry: self.geometry.clone(), mask_space: self.mask_space, decoded: None, alpha: self.alpha.clone() }
    }

    /// The surface while the frame still names the decoder's original picture.
    pub fn surface(&self) -> Option<&Arc<dyn DecodedSurface>> {
        self.decoded.as_ref().filter(|d| Weak::as_ptr(&d.of) == Arc::as_ptr(&self.data) && d.size == (self.width, self.height, self.format))?.surface.as_ref()
    }

    /// The packed RAM picture, made once by media when first needed, with this frame's stamps.
    pub fn materialise(&self) -> Frame {
        let Some(surface) = self.surface() else { return self.clone() };
        let mut frame = surface.ram().clone();
        (frame.number, frame.time_ns, frame.capture_ns, frame.phone_share) = (self.number, self.time_ns, self.capture_ns, self.phone_share);
        frame.geometry = self.geometry.clone();
        frame.mask_space = self.mask_space;
        frame.alpha = self.alpha.clone();
        frame
    }

    pub fn new(width: u32, height: u32, format: PixelFormat, data: Vec<u8>) -> Frame {
        debug_assert_eq!(data.len(), format.size(width, height));
        Frame { width, height, format, data: Arc::new(data), number: 0, time_ns: crate::time::monotonic_ns(), capture_ns: None, phone_share: None, geometry: None, mask_space: MaskSpace::default(), decoded: None, alpha: None }
    }

    /// The alpha plane, while it is the size of the picture: one left behind by a step that changed the size reads as none.
    pub fn alpha(&self) -> Option<&Arc<Alpha>> {
        self.alpha.as_ref().filter(|a| (a.w, a.h) == (self.width, self.height) && a.data.len() == a.w as usize * a.h as usize)
    }

    /// Gives the frame an alpha plane in place of any it had. False, and the frame left as it was, when the plane isn't the picture's size.
    pub fn set_alpha(&mut self, alpha: Arc<Alpha>) -> bool {
        let fits = (alpha.w, alpha.h) == (self.width, self.height) && alpha.data.len() == alpha.w as usize * alpha.h as usize;
        if fits {
            self.alpha = Some(alpha);
        }
        fits
    }

    /// Makes the frame all picture again: for a step that moves pixels or builds a new picture, which would leave the old plane misaligned.
    pub fn clear_alpha(&mut self) {
        self.alpha = None;
    }

    /// Makes the rectangle fully picture (alpha 255) for a step that draws there, so what it paints isn't flattened away. Clipped to the frame; nothing happens to a frame without alpha.
    pub fn opaque_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let Some(alpha) = self.alpha() else { return };
        let (fw, fh) = (alpha.w as usize, alpha.h as usize);
        let (x1, y1) = (x.saturating_add(w).min(fw), y.saturating_add(h).min(fh));
        if x >= x1 || y >= y1 {
            return;
        }
        let alpha = self.alpha.as_mut().unwrap();
        let plane = &mut Arc::make_mut(alpha).data;
        for row in y..y1 {
            plane[row * fw + x..row * fw + x1].fill(255);
        }
    }

    /// The frame with its alpha put over a flat background of this R, G, B colour, so nothing is left transparent. A frame with no alpha comes back as it is, sharing its pixels.
    pub fn flattened(&self, background: [u8; 3]) -> Frame {
        if self.surface().is_some() {
            return self.materialise().flattened(background);
        }
        let Some(alpha) = self.alpha().cloned() else { return self.clone() };
        if self.data.len() != self.format.size(self.width, self.height) {
            return self.clone();
        }
        let mut out = self.clone();
        out.alpha = None;
        if alpha.keep_opaque {
            return out;
        }
        let (w, h, premul) = (self.width as usize, self.height as usize, alpha.premultiplied);
        let a = &alpha.data[..];
        let data = out.data_mut();
        match self.format {
            PixelFormat::I420 | PixelFormat::Nv12 => {
                let [by, bu, bv] = yuv_of(background);
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                let (luma, chroma) = data.split_at_mut(w * h);
                crate::image::over_rows(luma, w, |y, row| {
                    for (x, p) in row.iter_mut().enumerate() {
                        *p = over(*p, a[y * w + x], by, premul);
                    }
                });
                // A chroma sample is as opaque as its block of pixels is on average
                let block = |cx: usize, cy: usize| {
                    let (x0, y0) = (cx * 2, cy * 2);
                    let (x1, y1) = ((x0 + 2).min(w), (y0 + 2).min(h));
                    let sum: u32 = (y0..y1).map(|y| a[y * w + x0..y * w + x1].iter().map(|&v| v as u32).sum::<u32>()).sum();
                    let n = ((x1 - x0) * (y1 - y0)) as u32;
                    ((sum + n / 2) / n) as u8
                };
                if self.format == PixelFormat::Nv12 {
                    crate::image::over_rows(chroma, cw * 2, |cy, row| {
                        for (cx, uv) in row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                            let ca = block(cx, cy);
                            uv[0] = over(uv[0], ca, bu, premul);
                            uv[1] = over(uv[1], ca, bv, premul);
                        }
                    });
                } else {
                    let (us, vs) = chroma.split_at_mut(cw * ch);
                    crate::image::over_rows(us, cw, |cy, row| row.iter_mut().enumerate().for_each(|(cx, p)| *p = over(*p, block(cx, cy), bu, premul)));
                    crate::image::over_rows(vs, cw, |cy, row| row.iter_mut().enumerate().for_each(|(cx, p)| *p = over(*p, block(cx, cy), bv, premul)));
                }
            }
            PixelFormat::Bgra | PixelFormat::Rgba => {
                let [r, g, b] = background;
                let behind = if self.format == PixelFormat::Bgra { [b, g, r] } else { [r, g, b] };
                crate::image::over_rows(data, w * 4, |y, row| {
                    for (x, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let pa = a[y * w + x];
                        for c in 0..3 {
                            p[c] = over(p[c], pa, behind[c], premul);
                        }
                        p[3] = 255;
                    }
                });
            }
        }
        out
    }

    /// The pixels to change, copied first if anything else still holds them.
    pub fn data_mut(&mut self) -> &mut Vec<u8> {
        if self.surface().is_some() {
            *self = self.materialise();
        }
        self.decoded = None;
        Arc::make_mut(&mut self.data)
    }

    /// Whole new pixels in place of the old ones, which stay as they were for anyone still holding them; with nobody else, the old buffer goes back to the spares.
    pub fn replace_data(&mut self, data: Vec<u8>) {
        debug_assert_eq!(data.len(), self.format.size(self.width, self.height));
        self.decoded = None;
        if let Ok(old) = Arc::try_unwrap(std::mem::replace(&mut self.data, Arc::new(data))) {
            crate::image::keep_bytes(old);
        }
    }

    /// Pixels that other frames share in place of the old ones, which are dropped with the decoder's BGR (it was made from them); a sole old buffer goes back to the spares.
    pub fn share_data(&mut self, data: Arc<Vec<u8>>) {
        debug_assert_eq!(data.len(), self.format.size(self.width, self.height));
        self.decoded = None;
        if let Ok(old) = Arc::try_unwrap(std::mem::replace(&mut self.data, data)) {
            crate::image::keep_bytes(old);
        }
    }

    /// Done with this frame: its pixels go back to the spares unless something else still holds them.
    pub fn recycle(self) {
        if let Ok(old) = Arc::try_unwrap(self.data) {
            crate::image::keep_bytes(old);
        }
    }

    /// The frame with the BGR its pixels were converted from, which effects read in place of the lossier pixels.
    pub fn with_bgr(self, picture: Arc<Bgr>) -> Frame {
        self.with_lazy_bgr(Arc::new(LazyBgr::ready(picture)))
    }

    /// The frame with a BGR that is made when something first reads it, and shared by every clone of the frame.
    pub fn with_lazy_bgr(mut self, picture: Arc<LazyBgr>) -> Frame {
        self.decoded = Some(Decoded { of: Arc::downgrade(&self.data), picture: Some(picture), surface: None, size: (self.width, self.height, self.format) });
        self
    }

    fn decoded_picture(&self) -> Option<&LazyBgr> {
        self.decoded.as_ref().filter(|d| Weak::as_ptr(&d.of) == Arc::as_ptr(&self.data)).and_then(|d| d.picture.as_deref())
    }

    /// Whether the frame has a BGR its pixels were made from, without making it if it isn't yet.
    pub fn has_bgr(&self) -> bool {
        self.decoded_picture().is_some()
    }

    /// The BGR this picture was made from, while the pixels are still that picture; made on the first call.
    pub fn bgr(&self) -> Option<&Bgr> {
        self.decoded_picture().map(LazyBgr::get).filter(|b| !b.data.is_empty())
    }

    /// The I420 planes (Y, U, V); None for other formats.
    pub fn i420_planes(&self) -> Option<(&[u8], &[u8], &[u8])> {
        if let Some(surface) = self.surface() {
            return surface.ram().i420_planes();
        }
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

    /// Whether the steps of this stage are there to hide something, so a failure of one must never let the clear picture out.
    pub fn hides(self) -> bool {
        matches!(self, FrameStage::Background | FrameStage::Privacy | FrameStage::Cover)
    }
}

/// What a frame step may ask while it works on a frame (HostFrames.frame_source, HostVision.person_mask, HostColour.colour_preview).
pub trait FrameContext {
    /// The source id of the stream this frame is from.
    fn source_id(&self) -> &str;
    /// The pipeline epoch (FrameStore::pipeline_epoch) when the steps began on this frame, None where the host doesn't say: a step that keeps
    /// frames for later marks them with it, not with the epoch of the moment it runs, since a change of steps may have come in between.
    fn entry_epoch(&self) -> Option<u64> {
        None
    }
    /// Whether the stream is being stopped, so a step that would wait on something slow (the graphics card) gives up and the frame is dropped. False where the host doesn't say.
    fn stopped(&self) -> bool {
        false
    }
    /// Whether the caller can't wait for the graphics card (the window's thread): a step that would have to wait for it is answered with a dark frame, and run for real later. False where the host doesn't say.
    fn hurried(&self) -> bool {
        false
    }
    /// How sure the vision service is each pixel is a person (0..255, the frame's size), or None while it can't say. It hardly ever waits for
    /// the model, so this is the newest mask there is, about a frame behind the picture. None also means the last one got too old, and a step
    /// that hides things has to stay safe on it.
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>>;
    /// person_mask at w x h instead of the frame's size, for a step that works smaller (a blur, a feathered edge): the mask is made at that size, so the full-size one is never built. The answer is None or kept as person_mask's is; the default shrinks person_mask's.
    fn person_mask_at(&self, frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        let mask = self.person_mask(frame)?;
        let (fw, fh) = (frame.width as usize, frame.height as usize);
        if mask.len() != fw * fh {
            return None;
        }
        if (w, h) == (fw, fh) {
            return Some(mask);
        }
        Some(Arc::new(crate::image::resize_area(&mask, fw, fh, 1, w, h)))
    }
    /// The same, from the slower model that cuts hair and edges cleaner (Background's High quality cutout). Without it set up, loaded or in
    /// time this is person_mask, so a step that asks for it never has less than the normal mask.
    fn person_mask_hq(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.person_mask(frame)
    }
    /// person_mask_hq at w x h, as person_mask_at is to person_mask.
    fn person_mask_hq_at(&self, frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        self.person_mask_at(frame, w, h)
    }
    /// Background's comfort cutout: keep the last mask while a fresh one is being made, mapped to this frame's crop.
    fn background_mask_at(&self, frame: &Frame, w: usize, h: usize, high_quality: bool) -> Option<Arc<Vec<u8>>> {
        if (w, h) == (frame.width as usize, frame.height as usize) {
            return if high_quality { self.person_mask_hq(frame) } else { self.person_mask(frame) };
        }
        if high_quality { self.person_mask_hq_at(frame, w, h) } else { self.person_mask_at(frame, w, h) }
    }
    /// For a final-stage step: the frame as the preview shows it, with a colour_split drawn in.
    fn colour_preview(&self, frame: &Frame) -> Frame;
    /// How hard the heavy effects should work: 0 is full, and 1 to 3 ask an effect that analyses the picture (a person model, a detector) to do it less often or smaller and to reuse its last result in between, a little more at each level. The app raises it while frames run late and lowers it when there is room. 0 where the host doesn't say.
    fn effort(&self) -> u8 {
        0
    }
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
    /// Whether this step is there to hide something (Privacy blur, Background, the wait screen). When it fails, the pipeline sends a dark frame
    /// in place of the one it was given, and keeps doing so if the step ends up skipped, rather than letting the clear picture through.
    fn hides(&self) -> bool {
        false
    }
    /// Whether process() lays a picture of its own over the whole frame, whatever the frame held (the wait screen). The pipeline then doesn't
    /// run the earlier steps that say `pixels_only`, since their work is thrown away.
    fn covers(&self) -> bool {
        false
    }
    /// Whether this step does nothing but change pixels: it reads no mask, tells nobody anything and keeps no picture or count of its own
    /// between frames, so a frame that gets covered anyway can go without it. Only a built-in colour or filter step says so.
    fn pixels_only(&self) -> bool {
        false
    }
    /// Asked right before process. Only a step that's certain may answer less than Changes.
    fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
        Touch::Changes
    }
    /// Told when the pipeline sends a dark frame in place of this step's work because it's skipped, without touch or process being asked. Only for a step that hides.
    fn withheld(&self, _ctx: &dyn FrameContext) {}
    /// For the CPU half of an Either: the shader's uniforms for a frame of this size, asked before each run on the card. A step whose
    /// uniforms depend on the picture's size (a warp worked out for it) answers here; the card only uploads an answer that changed.
    fn card_uniforms(&mut self, _width: u32, _height: u32) -> Option<Value> {
        None
    }
    /// For the CPU half of an Either: Rgba frames (red is the value) the shader samples by name, asked with the frame on the CPU; the card re-uploads one only when its data Arc changed, None means never any.
    fn card_textures(&mut self, _frame: &Frame, _ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
        None
    }
    /// For the CPU half of an Either: whether card_textures always has pictures to give (Some(true)), never does (Some(false)) or can't be told without asking (None). Some(true) is never asked on a frame the card is still ahead of, so a mask it works out is asked for once, on the picture the card really made.
    fn card_textures_offered(&self) -> Option<bool> {
        None
    }
    /// For the CPU half of an Either that makes a cut-out (Frame::alpha): the plane for a frame the card is about to work on, asked with the frame on the CPU. The card's shader never gives one, so a step whose CPU way does has to say so here.
    fn card_cut_out(&mut self, _frame: &Frame, _ctx: &dyn FrameContext) -> Option<Arc<Alpha>> {
        None
    }
    /// For the CPU half of an Either: what process changes on the frame besides its pixels (a view step's geometry), done on the frame in hand when the shader runs in its place.
    fn card_frame(&mut self, _frame: &mut Frame) {}
    /// For the CPU half of an Either: whether the card reads YUV chroma as the CPU does (a sample repeated over its 2 x 2 block, not blended).
    fn card_block_chroma(&self) -> bool {
        false
    }
    /// Whether the step leaves every pixel where it was (a colour change, a filter that works on each place alone), so the cut-out the frame
    /// carries (Frame::alpha) still fits. The pipeline drops it after an Effects or Cover step that doesn't say so.
    fn keeps_geometry(&self) -> bool {
        false
    }
    /// What the window's view steps answer with: a number that is the same for as long as the step puts the picture's pixels in the
    /// same places. A step that keeps pictures of its own downstream (denoising over time) starts again when it changes.
    fn geometry_key(&self) -> u64 {
        0
    }
    /// Called with the stream's source id before the first frame after the picture's geometry changed: what the step remembers of that
    /// stream's last pictures no longer lines up with the next one.
    fn forget_history(&mut self, _source: &str) {}
}

/// A frame step as a GPU shader, the only kind a Lua or WASM plugin hands over: plain data the host compiles and runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShaderStep {
    /// Whether the shader leaves every pixel where it was (a per-pixel colour change), so the cut-out the frame carries (Frame::alpha) still fits. False unless a built-in says so: the pipeline drops the cut-out after any other shader past Background.
    #[serde(default)]
    pub keeps_geometry: bool,
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
        if self.surface().is_some() {
            return self.materialise().serialize(s);
        }
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
    /// The same change two ways: the shader while the card has room, the CPU step while core::balance says it hasn't.
    Either(ShaderStep, Box<dyn CpuStep>),
}

impl std::fmt::Debug for FrameStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameStep::Cpu(_) => f.write_str("FrameStep::Cpu"),
            FrameStep::Shader(s) => f.debug_tuple("FrameStep::Shader").field(&s.wgsl.len()).finish(),
            FrameStep::Either(s, _) => f.debug_tuple("FrameStep::Either").field(&s.wgsl.len()).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_geometry_stays_with_frame_metadata() {
        #[derive(Debug)]
        struct Surface(Frame);

        impl DecodedSurface for Surface {
            fn ram(&self) -> &Frame { &self.0 }
            fn as_any(&self) -> &dyn std::any::Any { self }
        }

        let geometry = Some(Box::new(crate::view::Geometry { zoom: 2.0, pan_x: 0.5, ..Default::default() }));
        let pixels = Frame::new(2, 2, PixelFormat::I420, vec![1; 6]);
        let mut frame = Frame::from_surface(2, 2, PixelFormat::I420, Arc::new(Surface(pixels)));
        frame.geometry = geometry.clone();
        let mask_space = MaskSpace { source_size: (1920, 1080), source_epoch: 3, warp: Some(9), crop_unsure: false };
        frame.mask_space = mask_space;
        assert_eq!(frame.clone().geometry, geometry);
        assert_eq!(frame.without_pixels().geometry, geometry);
        assert_eq!(frame.materialise().geometry, geometry);
        assert_eq!(frame.clone().mask_space, mask_space);
        assert_eq!(frame.without_pixels().mask_space, mask_space);
        assert_eq!(frame.materialise().mask_space, mask_space);
        let json = serde_json::to_value(&frame).unwrap();
        assert!(json.get("geometry").is_none(), "source coordinates stay inside the frame pipeline");
        assert!(json.get("mask_space").is_none());
    }

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
    fn a_lazy_bgr_is_made_once_on_first_use_and_shared() {
        let made = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = made.clone();
        let lazy = Arc::new(LazyBgr::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Bgr { data: vec![7; 12], start: 0, stride: 6 }
        }));
        let frame = Frame::new(2, 2, PixelFormat::I420, vec![1; 6]).with_lazy_bgr(lazy);
        let clone = frame.clone();
        assert!(frame.has_bgr());
        assert_eq!(made.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing asked yet");
        assert_eq!(frame.bgr().unwrap().row(1, 2), [7; 6]);
        assert_eq!(clone.bgr().unwrap().row(0, 2), [7; 6]);
        assert_eq!(made.load(std::sync::atomic::Ordering::SeqCst), 1, "made once for both");
    }

    #[test]
    fn stages_sort_in_frame_order() {
        let mut s = FrameStage::ALL.to_vec();
        s.reverse();
        s.sort();
        assert_eq!(s, FrameStage::ALL);
        assert_eq!(serde_json::to_value(FrameStage::Colour).unwrap(), "colour");
    }

    #[test]
    fn replacing_the_pixels_leaves_other_holders_alone_and_recycles_a_sole_buffer() {
        let mut a = Frame::new(2, 2, PixelFormat::Rgba, vec![1; 16]);
        let b = a.clone();
        a.replace_data(vec![2; 16]);
        assert_eq!((a.data[0], b.data[0]), (2, 1));
        let mut lent = Vec::new();
        crate::image::with_spares(&mut lent, || {
            let mut sole = Frame::new(200, 100, PixelFormat::Rgba, vec![1; 80_000]);
            sole.replace_data(vec![3; 80_000]);
            assert_eq!(crate::image::spare_bytes(80_000).capacity(), 80_000, "the old buffer comes back out");
        });
    }

    fn with_alpha(mut f: Frame, a: u8, premultiplied: bool) -> Frame {
        let n = f.width as usize * f.height as usize;
        assert!(f.set_alpha(Arc::new(Alpha::new(f.width, f.height, vec![a; n], premultiplied))));
        f
    }

    #[test]
    fn the_alpha_stays_through_changes_to_the_pixels_and_clones_and_is_not_serialised() {
        let mut f = with_alpha(Frame::new(2, 2, PixelFormat::I420, vec![1; 6]), 90, false);
        let clone = f.clone();
        assert!(Arc::ptr_eq(f.alpha().unwrap(), clone.alpha().unwrap()), "a clone shares it");
        f.data_mut()[0] = 9;
        assert!(f.alpha().is_some());
        f.replace_data(vec![3; 6]);
        assert!(f.alpha().is_some());
        f.share_data(Arc::new(vec![4; 6]));
        assert_eq!(f.alpha().unwrap().data, [90; 4]);
        assert_ne!(f, clone, "a different picture");
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json.as_object().unwrap().keys().len(), 4);
        let back: Frame = serde_json::from_value(json).unwrap();
        assert!(back.alpha().is_none());
        f.clear_alpha();
        assert!(f.alpha().is_none());
    }

    #[test]
    fn an_alpha_plane_of_another_size_is_refused_and_one_left_behind_by_a_size_change_reads_as_none() {
        let mut f = Frame::new(4, 2, PixelFormat::Rgba, vec![0; 32]);
        assert!(!f.set_alpha(Arc::new(Alpha::new(2, 2, vec![255; 4], false))));
        assert!(f.alpha().is_none());
        let mut f = with_alpha(f, 255, false);
        (f.width, f.height) = (2, 2);
        f.replace_data(vec![0; 16]);
        assert!(f.alpha().is_none());
        assert!(f.flattened([0, 0, 0]).alpha().is_none());
    }

    #[test]
    fn flattening_a_frame_with_no_alpha_gives_the_same_pixels() {
        for format in [PixelFormat::I420, PixelFormat::Nv12, PixelFormat::Bgra, PixelFormat::Rgba] {
            let f = Frame::new(5, 3, format, (0..format.size(5, 3)).map(|i| i as u8).collect());
            let flat = f.flattened([200, 100, 50]);
            assert!(Arc::ptr_eq(&f.data, &flat.data), "{format:?} shares the pixels");
            assert_eq!(*flat.data, *f.data);
        }
    }

    #[test]
    fn an_empty_background_cutout_keeps_the_backdrop_only_on_opaque_outputs() {
        for format in [PixelFormat::I420, PixelFormat::Nv12, PixelFormat::Bgra, PixelFormat::Rgba] {
            let mut frame = Frame::new(4, 4, format, vec![180; format.size(4, 4)]);
            let ordinary = with_alpha(frame.clone(), 0, false).flattened([0, 0, 0]);
            let alpha = Alpha::new(4, 4, vec![0; 16], false).keep_opaque_when_empty();
            frame.set_alpha(Arc::new(alpha));
            let flat = frame.flattened([0, 0, 0]);
            assert!(frame.alpha().unwrap().data.iter().all(|&a| a == 0), "transparent outputs still have the empty cutout");
            assert!(flat.alpha().is_none());
            assert_eq!(flat.data, frame.data, "{format:?} keeps the rendered backdrop");
            assert_ne!(ordinary.data, flat.data, "other empty cutouts still flatten normally");
            let covered = Alpha::new(4, 4, vec![128; 16], false).keep_opaque_when_empty();
            frame.clear_alpha();
            frame.set_alpha(Arc::new(covered));
            assert_ne!(frame.flattened([0, 0, 0]).data, frame.data, "a visible person still flattens normally");
        }
    }

    #[test]
    fn flattening_puts_the_alpha_over_the_background_in_every_format() {
        let (w, h) = (4usize, 4usize);
        // Half of each pixel is the picture; the colour is stored as it is, or already multiplied by the alpha
        for premultiplied in [false, true] {
            let scale = |v: u8| if premultiplied { (v as u32 * 128 + 127) / 255 } else { v as u32 } as u8;
            // Either way it is half the colour over half the background, to within the rounding
            let expect = |c: u8, bg: u8| c as f32 * 128.0 / 255.0 + bg as f32 * 127.0 / 255.0;
            for (format, order) in [(PixelFormat::Rgba, [0, 1, 2]), (PixelFormat::Bgra, [2, 1, 0])] {
                let px = [200u8, 100, 50];
                let mut data = Vec::new();
                for _ in 0..w * h {
                    let mut p = [0u8, 0, 0, 77];
                    for c in 0..3 {
                        p[order[c]] = scale(px[c]);
                    }
                    data.extend(p);
                }
                let f = with_alpha(Frame::new(w as u32, h as u32, format, data), 128, premultiplied);
                let out = f.flattened([10, 20, 30]);
                assert!(out.alpha().is_none());
                for p in out.data.chunks(4) {
                    for (c, bg) in [(0, 10u8), (1, 20), (2, 30)] {
                        let got = p[order[c]] as f32;
                        assert!((got - expect(px[c], bg)).abs() <= 1.5, "{format:?} channel {c}: {got}");
                    }
                    assert_eq!(p[3], 255, "nothing is left transparent");
                }
            }
            // I420 and NV12 over black (Y 16, U and V 128): every plane value is blended on its own, chroma by its block's alpha
            for format in [PixelFormat::I420, PixelFormat::Nv12] {
                let n = format.size(w as u32, h as u32);
                let data: Vec<u8> = (0..n).map(|i| if i < w * h { scale(180) } else { scale(60) }).collect();
                let out = with_alpha(Frame::new(w as u32, h as u32, format, data), 128, premultiplied).flattened([0, 0, 0]);
                let want = |c: u8, bg: f32| c as f32 * 128.0 / 255.0 + bg * 127.0 / 255.0;
                for (i, v) in out.data.iter().enumerate() {
                    let expected = if i < w * h { want(180, 16.0) } else { want(60, 128.0) };
                    assert!((*v as f32 - expected).abs() <= 1.5, "{format:?} byte {i}: {v} against {expected}");
                }
            }
        }
    }

    #[test]
    fn a_chroma_sample_is_as_opaque_as_its_block_and_fully_opaque_pixels_are_untouched() {
        // 4 x 2: the left 2 x 2 block is the picture, the right one is all background
        let mut a = vec![255, 255, 0, 0, 255, 255, 0, 0];
        let f = Frame::new(4, 2, PixelFormat::I420, vec![200, 200, 200, 200, 200, 200, 200, 200, 60, 60, 90, 90]);
        let out = {
            let mut f = f.clone();
            assert!(f.set_alpha(Arc::new(Alpha::new(4, 2, a.clone(), false))));
            f.flattened([0, 0, 0])
        };
        assert_eq!(out.data[..8], [200, 200, 16, 16, 200, 200, 16, 16]);
        assert_eq!(out.data[8..], [60, 128, 90, 128]);
        a[0] = 0;
        let mut half = f.clone();
        half.set_alpha(Arc::new(Alpha::new(4, 2, a, false)));
        // one transparent pixel of the four: the chroma is three quarters picture
        let chroma = half.flattened([0, 0, 0]).data[8] as f32;
        assert!((chroma - (60.0 * 0.75 + 128.0 * 0.25)).abs() <= 1.0, "{chroma}");
    }

    #[test]
    fn opaque_rect_raises_only_its_rectangle_and_only_with_alpha() {
        let mut f = with_alpha(Frame::new(4, 3, PixelFormat::Rgba, vec![0; 48]), 0, false);
        f.opaque_rect(1, 1, 2, 5);
        let a = &f.alpha().unwrap().data;
        assert_eq!(a[..], [0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255, 0]);
        let mut shared = f.clone();
        shared.opaque_rect(0, 0, 1, 1);
        assert_eq!((f.alpha().unwrap().data[0], shared.alpha().unwrap().data[0]), (0, 255), "a copy doesn't change the original's plane");
        f.opaque_rect(9, 9, 2, 2);
        assert_eq!(f.alpha().unwrap().data.iter().filter(|&&v| v == 255).count(), 4);
        let mut plain = Frame::new(2, 2, PixelFormat::Rgba, vec![0; 16]);
        plain.opaque_rect(0, 0, 2, 2);
        assert!(plain.alpha().is_none());
    }

    #[test]
    fn a_background_colour_is_converted_for_the_yuv_planes() {
        let clear = with_alpha(Frame::new(2, 2, PixelFormat::I420, vec![90; 6]), 0, false);
        assert_eq!(*clear.flattened([255, 255, 255]).data, [235, 235, 235, 235, 128, 128]);
        assert_eq!(*clear.flattened([0, 0, 0]).data, [16, 16, 16, 16, 128, 128]);
        let red = clear.flattened([255, 0, 0]).data.clone();
        assert!(red[0].abs_diff(81) <= 1 && red[4].abs_diff(90) <= 1 && red[5].abs_diff(240) <= 1, "{red:?}");
    }
}
