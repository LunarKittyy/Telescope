//! The picture work around the models, as vision.py does it with OpenCV: shrinking a frame for a model, the tiny grey copy that
//! tells whether anything moved, carrying the last mask into the next, and turning a frame upright. Each one matches OpenCV's
//! bytes (tests/fixtures/vision.json has what Python made).

use telescope_core::image::{grey_of, resize_area, resize_linear_fixed};
use telescope_core::vision::Turn;

/// The segmenter's input (selfie_segmenter_landscape): MediaPipe shrinks anything bigger to it.
pub const MODEL_SIZE: (usize, usize) = (256, 144);
/// Moving: more than this share of a tiny grey copy changed by more than this many levels since the frame last used.
pub const MOVED_SHARE: f64 = 0.005;
pub const MOVED_LEVELS: u8 = 12;
pub const THUMB: (usize, usize) = (64, 36);

/// A picture as 8-bit R, G, B, row after row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Rgb {
    pub fn new(w: usize, h: usize, data: Vec<u8>) -> Rgb {
        debug_assert_eq!(data.len(), w * h * 3);
        Rgb { w, h, data }
    }

    pub fn filled(w: usize, h: usize, v: u8) -> Rgb {
        Rgb { w, h, data: vec![v; w * h * 3] }
    }

    /// cv2.resize(INTER_AREA).
    pub fn area(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: resize_area(&self.data, self.w, self.h, 3, w, h) }
    }

    /// cv2.resize(INTER_LINEAR).
    pub fn linear(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: resize_linear_fixed(&self.data, self.w, self.h, 3, w, h) }
    }
}

/// Python's round(): halves go to the even number.
pub fn py_round(v: f64) -> usize {
    v.round_ties_even().max(0.0) as usize
}

/// The size model_input shrinks a w x h frame to; None when it's small enough already.
pub fn model_input_size(w: usize, h: usize, size: (usize, usize)) -> Option<(usize, usize)> {
    let scale = (size.0 as f64 / w as f64).max(size.1 as f64 / h as f64);
    if scale >= 1.0 {
        return None;
    }
    Some((py_round(w as f64 * scale).max(1), py_round(h as f64 * scale).max(1)))
}

/// The frame shrunk to just cover size, keeping its shape (vision.model_input). A linear step to twice that and an area step
/// from there looks like one area step at a fraction of the cost. None when it's small enough already.
pub fn model_input(frame: &Rgb, size: (usize, usize)) -> Option<Rgb> {
    let (w, h) = (frame.w, frame.h);
    let out = model_input_size(w, h, size)?;
    if out.0 * 2 < w && out.1 * 2 < h {
        return Some(frame.linear(out.0 * 2, out.1 * 2).area(out.0, out.1));
    }
    Some(frame.area(out.0, out.1))
}

/// The size feed_raw shrinks a frame to for the pose model: at most `most` wide.
pub fn people_size(w: usize, h: usize, most: usize) -> (usize, usize) {
    let width = most.min(w);
    (width, py_round(h as f64 * width as f64 / w as f64).max(1))
}

/// The tiny grey copy _Motion compares: every step-th pixel, shrunk by area to THUMB, then grey.
pub fn thumb(frame: &Rgb) -> Vec<u8> {
    let step = (frame.w / (THUMB.0 * 4)).max(1);
    let (sw, sh) = (frame.w.div_ceil(step), frame.h.div_ceil(step));
    let mut picked = Vec::with_capacity(sw * sh * 3);
    for y in (0..frame.h).step_by(step) {
        for x in (0..frame.w).step_by(step) {
            let i = (y * frame.w + x) * 3;
            picked.extend_from_slice(&frame.data[i..i + 3]);
        }
    }
    let small = resize_area(&picked, sw, sh, 3, THUMB.0, THUMB.1);
    small.as_chunks::<3>().0.iter().map(|p| grey_of(p[0], p[1], p[2])).collect()
}

/// Whether frames still look like the one last used, from a tiny grey copy (cheap enough for every frame).
#[derive(Clone, Debug, Default)]
pub struct Motion {
    used: Option<Vec<u8>>,
    now: Option<Vec<u8>>,
}

impl Motion {
    pub fn moved(&mut self, frame: &Rgb) -> bool {
        let now = thumb(frame);
        let moved = match &self.used {
            None => true,
            Some(used) => {
                let changed = now.iter().zip(used).filter(|(a, b)| a.abs_diff(**b) > MOVED_LEVELS).count();
                changed as f64 > MOVED_SHARE * now.len() as f64
            }
        };
        self.now = Some(now);
        moved
    }

    /// The frame moved() last looked at is the one used: later frames are compared with it.
    pub fn used(&mut self) {
        self.used = self.now.clone();
    }
}

/// cv2.addWeighted(a, alpha, b, beta, 0) on 8-bit masks: OpenCV's vector path, a fused multiply-add in f32, rounded to even.
pub fn add_weighted(a: &[u8], alpha: f64, b: &[u8], beta: f64) -> Vec<u8> {
    let (alpha, beta) = (alpha as f32, beta as f32);
    a.iter().zip(b).map(|(&x, &y)| {
        let v = (x as f32).mul_add(alpha, (y as f32).mul_add(beta, 0.0));
        v.round_ties_even().clamp(0.0, 255.0) as u8
    }).collect()
}

/// cv2.convertScaleAbs(mask, alpha=scale): |v * scale| rounded to even and saturated.
pub fn convert_scale_abs(values: &[f32], scale: f64) -> Vec<u8> {
    let scale = scale as f32;
    values.iter().map(|v| (v * scale).abs().round_ties_even().clamp(0.0, 255.0) as u8).collect()
}

/// A mask (one byte a pixel) scaled with cv2.resize(INTER_LINEAR).
pub fn mask_resized(mask: &[u8], w: usize, h: usize, w2: usize, h2: usize) -> Vec<u8> {
    resize_linear_fixed(mask, w, h, 1, w2, h2)
}

/// view.transform_frame: the flips (cv2.flip), then the turn (cv2.rotate), degrees clockwise.
pub fn turned(frame: &Rgb, turn: Turn) -> Rgb {
    let (w, h) = (frame.w, frame.h);
    let px = |x: usize, y: usize| {
        let (x, y) = (if turn.flip_h { w - 1 - x } else { x }, if turn.flip_v { h - 1 - y } else { y });
        &frame.data[(y * w + x) * 3..(y * w + x) * 3 + 3]
    };
    let (ow, oh) = match turn.rotation {
        Some(90) | Some(270) => (h, w),
        _ => (w, h),
    };
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match turn.rotation {
                Some(90) => (y, h - 1 - x),
                Some(180) => (w - 1 - x, h - 1 - y),
                Some(270) => (w - 1 - y, x),
                _ => (x, y),
            };
            out.extend_from_slice(px(sx, sy));
        }
    }
    Rgb::new(ow, oh, out)
}
