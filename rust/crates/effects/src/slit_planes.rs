//! Slit-scan on the planes of the frame itself. A frame's chroma planes are already half the picture's size, which is the size the
//! effect works at, so the colour is kept as it is and only the luma is averaged down and blown up again: no trip through RGB.

use crate::fun_effects::band_rows;
use std::collections::VecDeque;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::{keep_bytes, resize_area, resize_linear_into, spare_bytes};

/// One frame at half size: luma, and the two chroma planes as they came.
struct Small {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Small {
    fn recycle(self) {
        [self.y, self.u, self.v].into_iter().for_each(keep_bytes);
    }
}

#[derive(Default)]
pub struct History {
    frames: VecDeque<Small>,
    shape: Option<(usize, usize)>,
}

impl History {
    pub fn clear(&mut self) {
        self.frames.drain(..).for_each(Small::recycle);
        self.shape = None;
    }

    /// The frame with its bands taken from older frames; None for a frame this doesn't work on (an odd size, a packed format), left as it was.
    pub fn run(&mut self, frame: &mut Frame, across: bool, spread: usize) -> Option<()> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let nv12 = frame.format == PixelFormat::Nv12;
        if !(nv12 || frame.format == PixelFormat::I420) || w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 {
            return None;
        }
        let (sw, sh) = (w / 2, h / 2);
        let cells = sw * sh;
        if frame.data.len() < w * h + 2 * cells {
            return None;
        }
        if self.shape != Some((sw, sh)) {
            self.clear();
            self.shape = Some((sw, sh));
        }
        let (luma, chroma) = frame.data.split_at(w * h);
        let (mut u, mut v) = (spare_bytes(cells), spare_bytes(cells));
        if nv12 {
            for ((u, v), pair) in u.iter_mut().zip(v.iter_mut()).zip(chroma[..2 * cells].as_chunks::<2>().0) {
                (*u, *v) = (pair[0], pair[1]);
            }
        } else {
            u.copy_from_slice(&chroma[..cells]);
            v.copy_from_slice(&chroma[cells..2 * cells]);
        }
        self.frames.push_front(Small { y: resize_area(luma, w, h, 1, sw, sh), u, v });
        while self.frames.len() > spread + 1 {
            if let Some(old) = self.frames.pop_back() {
                old.recycle();
            }
        }
        let count = self.frames.len();
        let [mut by, mut bu, mut bv] = [0; 3].map(|_| spare_bytes(cells));
        band_rows(&mut by, 1, &|i| &self.frames[i].y, count, across, spread, sw, sh);
        band_rows(&mut bu, 1, &|i| &self.frames[i].u, count, across, spread, sw, sh);
        band_rows(&mut bv, 1, &|i| &self.frames[i].v, count, across, spread, sw, sh);
        let mut out = spare_bytes(w * h + 2 * cells);
        resize_linear_into(&by, sw, sh, 1, &mut out[..w * h], w, (0, 0, w, h));
        let chroma = &mut out[w * h..];
        if nv12 {
            for (pair, (u, v)) in chroma.as_chunks_mut::<2>().0.iter_mut().zip(bu.iter().zip(&bv)) {
                *pair = [*u, *v];
            }
        } else {
            chroma[..cells].copy_from_slice(&bu);
            chroma[cells..].copy_from_slice(&bv);
        }
        [by, bu, bv].into_iter().for_each(keep_bytes);
        frame.replace_data(out);
        Some(())
    }
}

#[cfg(test)]
#[path = "slit_planes_tests.rs"]
mod tests;
