//! The picture work around the models, as the Python app did it with OpenCV: shrinking a frame for a model, the tiny grey copy that
//! tells whether anything moved, carrying the last mask into the next, and turning a frame upright. Each one matches OpenCV's
//! bytes (tests/fixtures/vision.json has what Python made).

use telescope_core::image::{grey_of, resize_area, resize_linear_fixed};
use telescope_core::vision::Turn;

/// The segmenter's input (selfie_segmenter_landscape): MediaPipe shrinks anything bigger to it.
pub const MODEL_SIZE: (usize, usize) = (256, 144);
/// Moving: more than this share of a tiny grey copy changed by more than this many levels since the frame last used.
pub const MOVED_SHARE: f64 = 0.005;
pub const MOVED_LEVELS: u8 = 12;
/// A share of the tiny grey copy this changed since the mask was made is a big move (standing up, a wave across the picture).
pub const BIG_MOVE_SHARE: f64 = 0.1;
pub const THUMB: (usize, usize) = (64, 36);
/// The cells of a chroma plane a frame's look has beside its grey one, for a change of colour that keeps the luma.
pub const CHROMA_GRID: (usize, usize) = (32, 18);

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
    /// Whether the frame moved() last looked at changed by BIG_MOVE_SHARE or more.
    big: bool,
}

impl Motion {
    pub fn moved(&mut self, frame: &Rgb) -> bool {
        self.moved_grey(thumb(frame))
    }

    /// moved() for a thumb already made as grey (small_grey of THUMB size): what the people thread's frames are compared by, so a frame that goes no further is never made into a colour picture.
    pub fn moved_grey(&mut self, now: Vec<u8>) -> bool {
        self.big = true;
        let moved = match &self.used {
            None => true,
            Some(used) if used.len() != now.len() => true,
            Some(used) => {
                // The grey cells and the chroma cells after them are each judged by their own share, so the chroma ones don't dilute a small moving patch
                let grey = (THUMB.0 * THUMB.1).min(now.len());
                let (now_grey, now_chroma) = now.split_at(grey);
                let (used_grey, used_chroma) = used.split_at(grey);
                let share = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(a, b)| a.abs_diff(**b) > MOVED_LEVELS).count() as f64 / a.len().max(1) as f64;
                let (grey, chroma) = (share(now_grey, used_grey), share(now_chroma, used_chroma));
                self.big = grey >= BIG_MOVE_SHARE;
                grey > MOVED_SHARE || chroma > MOVED_SHARE
            }
        };
        self.now = Some(now);
        moved
    }

    /// Whether the frame moved() last looked at changed a lot since the one used.
    pub fn big(&self) -> bool {
        self.big
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

/// A pixel at least this sure (of 255) is plainly a person.
const FIRM_CONFIDENT: u8 = 200;
/// How far (in model pixels) from a plainly-person pixel a doubtful one still counts as part of the same body.
const FIRM_REACH: usize = 8;
/// Below this the model is only guessing at a stray speck: it goes.
const FIRM_FLOOR: u8 = 20;
/// A pixel at least this sure counts towards being inside a body, and the window it is counted in reaches this far each way.
const FIRM_SOME: u8 = 40;
const FIRM_WINDOW: usize = 4;
/// Inside means this share of the window is at least FIRM_SOME.
const FIRM_INSIDE: f64 = 0.85;

/// For each pixel, how many pixels within `reach` of it each way (a square window, the picture's edge cutting it short) pass `mark`:
/// a running sum along each row and then down each column, so the work doesn't grow with the reach (a mask thread can't be held up).
fn counts(mask: &[u8], w: usize, h: usize, mark: impl Fn(u8) -> bool, reach: usize) -> Vec<u32> {
    let mut across = vec![0u32; w * h];
    let mut sum = vec![0u32; w.max(h) + 1];
    for y in 0..h {
        for x in 0..w {
            sum[x + 1] = sum[x] + mark(mask[y * w + x]) as u32;
        }
        for x in 0..w {
            across[y * w + x] = sum[(x + reach + 1).min(w)] - sum[x.saturating_sub(reach)];
        }
    }
    let mut out = vec![0u32; w * h];
    for x in 0..w {
        for y in 0..h {
            sum[y + 1] = sum[y] + across[y * w + x];
        }
        for y in 0..h {
            out[y * w + x] = sum[(y + reach + 1).min(h)] - sum[y.saturating_sub(reach)];
        }
    }
    out
}

/// The model's confidence mask with the doubtful middle made solid where it is inside a body: a broad stretch of faint-but-not-nothing
/// that sits right next to a plainly-person part of the picture, as pale clothes and chair backs do (the part of a mask a new
/// background shows through). Faint specks away from anyone are dropped. Thin edges, flyaway hair and anything far from a confident
/// part are left as they were, so outlines don't grow. Run on each fresh mask before it is blended with the last one, so an old
/// hand's silhouette is never firmed.
pub fn firm_mask(mask: &[u8], w: usize, h: usize) -> Vec<u8> {
    if mask.len() != w * h || w == 0 || h == 0 {
        return mask.to_vec();
    }
    let (near, some) = (counts(mask, w, h, |v| v >= FIRM_CONFIDENT, FIRM_REACH), counts(mask, w, h, |v| v >= FIRM_SOME, FIRM_WINDOW));
    let window = ((2 * FIRM_WINDOW + 1) * (2 * FIRM_WINDOW + 1)) as f64;
    (0..w * h).map(|i| {
        let v = mask[i];
        if v < FIRM_FLOOR {
            0
        } else if v >= FIRM_SOME && near[i] > 0 && some[i] as f64 / window >= FIRM_INSIDE {
            255
        } else {
            v
        }
    }).collect()
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

#[cfg(test)]
mod firm_tests {
    use super::firm_mask;

    fn at(mask: &[u8], w: usize, x: usize, y: usize) -> u8 {
        mask[y * w + x]
    }

    #[test]
    fn a_half_sure_panel_beside_a_sure_body_is_made_solid() {
        let (w, h) = (64, 32);
        let mut mask = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                mask[y * w + x] = if x < 20 { 255 } else if x < 40 { 60 } else { 0 };
            }
        }
        let out = firm_mask(&mask, w, h);
        // well inside the panel and within the reach of the body: solid
        assert_eq!(at(&out, w, 24, 10), 255);
        // its last pixels before the edge keep their softness
        assert_eq!(at(&out, w, 39, 10), 60);
        // the same panel's far end isn't near anything sure
        assert_eq!(at(&out, w, 38, 10), 60);
        assert_eq!(at(&out, w, 5, 10), 255);
        assert_eq!(at(&out, w, 50, 10), 0);
    }

    #[test]
    fn a_thin_soft_edge_is_not_grown() {
        let (w, h) = (64, 32);
        let mask: Vec<u8> = (0..w * h).map(|i| match i % w { x if x < 20 => 255, x if x < 24 => 100, _ => 0 }).collect();
        let out = firm_mask(&mask, w, h);
        assert_eq!(at(&out, w, 21, 10), 100);
        assert_eq!(at(&out, w, 23, 10), 100);
    }

    #[test]
    fn a_far_speck_goes_and_a_far_doubtful_part_is_left_alone() {
        let (w, h) = (64, 32);
        let mut mask = vec![0u8; w * h];
        mask[16 * w + 50] = 10;
        mask[4 * w + 4] = 60;
        let out = firm_mask(&mask, w, h);
        assert_eq!(at(&out, w, 50, 16), 0);
        assert_eq!(at(&out, w, 4, 4), 60);
    }

    #[test]
    fn nothing_changes_for_a_mask_that_is_already_sure_or_empty() {
        let sure: Vec<u8> = (0..64 * 32).map(|i| if i % 64 < 32 { 255 } else { 0 }).collect();
        assert_eq!(firm_mask(&sure, 64, 32), sure);
        assert_eq!(firm_mask(&vec![0; 64 * 32], 64, 32), vec![0; 64 * 32]);
        // wrong size: handed back as it was
        assert_eq!(firm_mask(&[1, 2, 3], 4, 4), vec![1, 2, 3]);
    }
}
