//! The pixel work of the window's view (view.apply_zoom, transform_frame and apply_stretch), as frame steps for the View stage.
//!
//! Python crops, flips, turns and then stretches the picture. All of it runs on the CPU, so a card that OBS is encoding on can't hold
//! up a frame for a zoom: a quarter turn is an exact copy of the planes, a flip is the planes copied in another order, and a zoom
//! or a stretch is one bilinear resampling of the picture as RGB. The zoom is a fractional crop that repeats the edge, the stretch
//! leaves black where the picture doesn't reach, both like cv2.warpAffine's linear interpolation does.

use crate::frame::{CpuStep, Frame, FrameContext, FrameStep, PixelFormat};
use crate::host::View;
use crate::image::{frame_rgb, keep_bytes, resample_linear, rgb_to_420, rint, Tap};
use crate::view::window;

/// Quarter turns clockwise in a rotation of degrees (0 for none).
fn quarters(rotation: Option<i32>) -> u32 {
    match rotation {
        Some(90) => 1,
        Some(180) => 2,
        Some(270) => 3,
        _ => 0,
    }
}

/// A quarter turn of the whole picture, planes copied across (no resampling).
struct Turn {
    quarters: u32,
}

/// `plane` (w x h, `bpp` bytes a pixel) turned clockwise by 1 or 3 quarters: h x w.
fn turn_plane(plane: &[u8], w: usize, h: usize, bpp: usize, quarters: u32) -> Vec<u8> {
    let mut out = vec![0u8; plane.len()];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = if quarters == 1 { (h - 1 - y, x) } else { (y, w - 1 - x) };
            let (from, to) = ((y * w + x) * bpp, (ny * h + nx) * bpp);
            out[to..to + bpp].copy_from_slice(&plane[from..from + bpp]);
        }
    }
    out
}

/// The frame turned by a quarter or three (clockwise); the same frame for other counts or formats it can't turn.
pub fn turn_frame(frame: &Frame, quarters: u32) -> Frame {
    if quarters % 4 != 1 && quarters % 4 != 3 {
        return frame.clone();
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let data: Vec<u8> = match frame.format {
        PixelFormat::Rgba | PixelFormat::Bgra => turn_plane(&frame.data, w, h, 4, quarters % 4),
        PixelFormat::I420 => {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let (y, u, v) = frame.i420_planes().expect("an I420 frame has planes");
            [turn_plane(y, w, h, 1, quarters % 4), turn_plane(u, cw, ch, 1, quarters % 4), turn_plane(v, cw, ch, 1, quarters % 4)].concat()
        }
        PixelFormat::Nv12 => {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let (y, uv) = frame.data.split_at(w * h);
            [turn_plane(y, w, h, 1, quarters % 4), turn_plane(uv, cw, ch, 2, quarters % 4)].concat()
        }
    };
    let mut out = Frame::new(frame.height, frame.width, frame.format, data);
    out.number = frame.number;
    out.time_ns = frame.time_ns;
    out
}

impl CpuStep for Turn {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        *frame = turn_frame(frame, self.quarters);
    }
}

/// A mirror, a flip or a half turn: the planes copied across in another order, so the picture never has to visit the graphics card.
struct Flip {
    horizontal: bool,
    vertical: bool,
}

/// `plane` (w x h, `bpp` bytes a pixel) flipped left to right and/or top to bottom, in place.
fn flip_plane(plane: &mut [u8], w: usize, h: usize, bpp: usize, horizontal: bool, vertical: bool) {
    let row = w * bpp;
    if horizontal && row > 0 {
        for line in plane.chunks_exact_mut(row) {
            if bpp == 1 {
                line.reverse();
            } else {
                for x in 0..w / 2 {
                    for b in 0..bpp {
                        line.swap(x * bpp + b, (w - 1 - x) * bpp + b);
                    }
                }
            }
        }
    }
    if vertical {
        for y in 0..h / 2 {
            let (top, bottom) = plane.split_at_mut((h - 1 - y) * row);
            top[y * row..(y + 1) * row].swap_with_slice(&mut bottom[..row]);
        }
    }
}

/// The frame flipped in place.
pub fn flip_frame(frame: &mut Frame, horizontal: bool, vertical: bool) {
    if !horizontal && !vertical {
        return;
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let format = frame.format;
    let data = frame.data_mut();
    match format {
        PixelFormat::Rgba | PixelFormat::Bgra => flip_plane(data, w, h, 4, horizontal, vertical),
        PixelFormat::I420 => {
            let (y, rest) = data.split_at_mut(w * h);
            let (u, v) = rest.split_at_mut(cw * ch);
            flip_plane(y, w, h, 1, horizontal, vertical);
            flip_plane(u, cw, ch, 1, horizontal, vertical);
            flip_plane(v, cw, ch, 1, horizontal, vertical);
        }
        PixelFormat::Nv12 => {
            let (y, uv) = data.split_at_mut(w * h);
            flip_plane(y, w, h, 1, horizontal, vertical);
            flip_plane(uv, cw, ch, 2, horizontal, vertical);
        }
    }
}

impl CpuStep for Flip {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        flip_frame(frame, self.horizontal, self.vertical);
    }
}

/// One resampling of the picture as given (turned by `done` quarters already): the crop, flips and half turn of the view, or its
/// stretch. Python does these in two resamplings, the stretch of the 8-bit result of the crop, and so does this, so thin lines blur
/// the same amount.
#[derive(Clone, Copy, Debug)]
struct Pass {
    x0: f64,
    y0: f64,
    crop_w: f64,
    crop_h: f64,
    flip_h: bool,
    flip_v: bool,
    stretch_x: f64,
    stretch_y: f64,
    turn: u32,
    done: u32,
}

fn unturn(p: (f64, f64), quarters: u32) -> (f64, f64) {
    match quarters {
        1 => (p.1, 1.0 - p.0),
        2 => (1.0 - p.0, 1.0 - p.1),
        3 => (1.0 - p.1, p.0),
        _ => p,
    }
}

fn turned(p: (f64, f64), quarters: u32) -> (f64, f64) {
    match quarters {
        1 => (1.0 - p.1, p.0),
        2 => (1.0 - p.0, 1.0 - p.1),
        3 => (p.1, 1.0 - p.0),
        _ => p,
    }
}

impl Pass {
    /// `zoom` the crop, flips and half turn of `view`, `stretch` its stretch.
    fn new(view: &View, done: u32, zoom: bool, stretch: bool) -> Pass {
        let (z, pan_x, pan_y) = if zoom { (view.zoom as f64, (view.pan_x as f64).clamp(-1.0, 1.0), (view.pan_y as f64).clamp(-1.0, 1.0)) } else { (1.0, 0.0, 0.0) };
        let ((x0, crop_w), (y0, crop_h)) = (window(z, pan_x), window(z, pan_y));
        let (stretch_x, stretch_y) = if stretch { (view.stretch_x as f64, view.stretch_y as f64) } else { (1.0, 1.0) };
        Pass { x0, y0, crop_w, crop_h, flip_h: zoom && view.flip_h, flip_v: zoom && view.flip_v, stretch_x, stretch_y,
               turn: if zoom { quarters(view.rotation) } else { 0 }, done: if zoom { done } else { 0 } }
    }

    /// The point of the stretched picture (0..1) an output point (0..1) shows.
    fn stretched(&self, u: f64, v: f64) -> (f64, f64) {
        (0.5 + (u - 0.5) / self.stretch_x, 0.5 + (v - 0.5) / self.stretch_y)
    }

    /// Where in the source (0..1) the output point (u, v) is taken from. Each axis of the output reads one axis of the source,
    /// whatever the turn, so the two can be worked out apart.
    fn source(&self, u: f64, v: f64) -> (f64, f64) {
        let mut p = unturn(self.stretched(u, v), self.turn);
        if self.flip_h {
            p.0 = 1.0 - p.0;
        }
        if self.flip_v {
            p.1 = 1.0 - p.1;
        }
        turned((self.x0 + p.0 * self.crop_w, self.y0 + p.1 * self.crop_h), self.done)
    }

    fn is_stretch(&self) -> bool {
        (self.stretch_x - 1.0).abs() >= 1e-6 || (self.stretch_y - 1.0).abs() >= 1e-6
    }
}

/// The two taps of a bilinear sample at `at` (in pixels, the first pixel's centre at 0) from n pixels, the edge repeated.
fn tap(at: f32, n: usize) -> Tap {
    let last = n as i64 - 1;
    let s = at.floor();
    let q = rint(((at - s) * 2048.0) as f64);
    let s = s as i64;
    (s.clamp(0, last) as usize, (s + 1).clamp(0, last) as usize, 2048 - q, q)
}

/// How much of a pixel a stretched picture covers along an axis: 1 inside, fading to 0 over half a pixel past its edge.
fn covered(s: f64, n: usize) -> f32 {
    let at = s * n as f64;
    ((at + 0.5).clamp(0.0, 1.0) * (n as f64 - at + 0.5).clamp(0.0, 1.0)) as f32
}

/// Zoom and stretch: the frame resampled as RGB, then back in its own format.
struct Resample {
    passes: Vec<Pass>,
}

/// `pixels` (RGB, w x h) through one pass, the same size.
fn resample(pixels: Vec<u8>, w: usize, h: usize, pass: &Pass) -> Vec<u8> {
    let xs: Vec<Tap> = (0..w).map(|x| tap((pass.source((x as f64 + 0.5) / w as f64, 0.5).0 * w as f64 - 0.5) as f32, w)).collect();
    let ys: Vec<Tap> = (0..h).map(|y| tap((pass.source(0.5, (y as f64 + 0.5) / h as f64).1 * h as f64 - 0.5) as f32, h)).collect();
    let mut out = resample_linear(&pixels, w, h, 3, &xs, &ys);
    keep_bytes(pixels);
    if pass.is_stretch() {
        black_outside(&mut out, w, h, pass);
    }
    out
}

/// What a stretch leaves bare: black, fading in over the pixel at the picture's edge.
fn black_outside(rgb: &mut [u8], w: usize, h: usize, pass: &Pass) {
    let fx: Vec<f32> = (0..w).map(|x| covered(pass.stretched((x as f64 + 0.5) / w as f64, 0.5).0, w)).collect();
    let fy: Vec<f32> = (0..h).map(|y| covered(pass.stretched(0.5, (y as f64 + 0.5) / h as f64).1, h)).collect();
    // The columns at either end that aren't wholly inside; the middle only ever changes with its row
    let left = fx.iter().take_while(|&&f| f < 1.0).count();
    let right = w - fx.iter().rev().take_while(|&&f| f < 1.0).count();
    let (left, right) = (left.min(w), right.max(left));
    let scale = |px: &mut [u8], f: f32| px.iter_mut().for_each(|v| *v = (*v as f32 * f + 0.5) as u8);
    for (line, &f) in rgb.chunks_exact_mut(w * 3).zip(&fy) {
        if f == 0.0 {
            line.fill(0);
            continue;
        }
        for x in (0..left).chain(right..w) {
            scale(&mut line[x * 3..][..3], fx[x] * f);
        }
        if f < 1.0 {
            scale(&mut line[left * 3..right * 3], f);
        }
    }
}

impl CpuStep for Resample {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let Some(mut rgb) = frame_rgb(frame) else { return };
        for pass in &self.passes {
            rgb = resample(rgb, w, h, pass);
        }
        let format = frame.format;
        let data = frame.data_mut();
        match format {
            PixelFormat::Rgba => data.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).for_each(|(o, p)| *o = [p[0], p[1], p[2], 255]),
            PixelFormat::Bgra => data.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).for_each(|(o, p)| *o = [p[2], p[1], p[0], 255]),
            PixelFormat::I420 => rgb_to_420(&rgb, w, h, data, false),
            PixelFormat::Nv12 => rgb_to_420(&rgb, w, h, data, true),
        }
        keep_bytes(rgb);
    }
}

/// Whether the crop, flips or a half turn of the view change the picture.
fn crop_needed(view: &View) -> bool {
    view.zoom > 1.0 || view.flip_h || view.flip_v || quarters(view.rotation) == 2
}

fn stretch_needed(view: &View) -> bool {
    (view.stretch_x - 1.0).abs() >= 1e-6 || (view.stretch_y - 1.0).abs() >= 1e-6
}

/// The frame steps of a view, in order, all on the CPU (a busy card, OBS encoding on it, would hold up every frame for a shader, and
/// a shader step can't move to the CPU): a quarter turn when the picture changes shape, then either a plain copy for a view that only
/// mirrors, flips or half turns, or one resampling for the crop and another for the stretch. No steps for a view that changes nothing.
pub fn steps(view: &View) -> Vec<FrameStep> {
    let q = quarters(view.rotation);
    let mut out: Vec<FrameStep> = Vec::new();
    // Turned first, the rest of the work is the same as for any other view, on the turned picture
    let done = if q == 1 || q == 3 { q } else { 0 };
    if done != 0 {
        out.push(FrameStep::Cpu(Box::new(Turn { quarters: done })));
    }
    if crop_needed(view) && done == 0 && view.zoom <= 1.0 && !stretch_needed(view) {
        // A half turn is both flips, and a second flip undoes the first
        let half = q == 2;
        out.push(FrameStep::Cpu(Box::new(Flip { horizontal: view.flip_h != half, vertical: view.flip_v != half })));
        return out;
    }
    let mut passes = Vec::new();
    if crop_needed(view) {
        passes.push(Pass::new(view, done, true, false));
    }
    if stretch_needed(view) {
        passes.push(Pass::new(view, 0, false, true));
    }
    if !passes.is_empty() {
        out.push(FrameStep::Cpu(Box::new(Resample { passes })));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(w: u32, h: u32) -> Frame {
        Frame::new(w, h, PixelFormat::Rgba, (0..w * h).flat_map(|i| [i as u8, 0, 0, 255]).collect())
    }

    #[test]
    fn a_view_that_changes_nothing_has_no_steps() {
        assert!(steps(&View::default()).is_empty());
        assert!(steps(&View { zoom: 0.5, ..View::default() }).is_empty(), "at or below 1x the frame goes through untouched");
        assert!(steps(&View { zoom: 1.0, pan_x: 0.7, ..View::default() }).is_empty(), "pan counts only once zoomed in");
    }

    #[test]
    fn a_zoom_or_a_stretch_never_needs_the_graphics_card_either() {
        for view in [View { zoom: 2.0, ..View::default() }, View { stretch_x: 0.8, ..View::default() }, View { flip_h: true, zoom: 2.0, ..View::default() },
                     View { zoom: 1.5, stretch_y: 1.2, ..View::default() }, View { rotation: Some(90), zoom: 2.0, ..View::default() }] {
            let s = steps(&view);
            assert!(!s.is_empty() && s.iter().all(|step| matches!(step, FrameStep::Cpu(_))), "{view:?}");
        }
        assert_eq!(steps(&View { zoom: 1.5, stretch_y: 1.2, ..View::default() }).len(), 1, "a crop and a stretch share one trip through RGB");
    }

    #[test]
    fn a_mirror_a_flip_or_a_half_turn_alone_never_needs_the_graphics_card() {
        for view in [View { flip_h: true, ..View::default() }, View { flip_v: true, ..View::default() }, View { rotation: Some(180), ..View::default() },
                     View { rotation: Some(180), flip_h: true, flip_v: true, ..View::default() }] {
            let s = steps(&view);
            assert!(s.len() == 1 && matches!(s[0], FrameStep::Cpu(_)), "{view:?}");
        }
    }

    struct NoCtx;

    impl FrameContext for NoCtx {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<std::sync::Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }

    fn through(view: &View, mut frame: Frame) -> Frame {
        for step in steps(view) {
            let FrameStep::Cpu(mut cpu) = step else { panic!("a CPU step") };
            cpu.process(&mut frame, &NoCtx);
        }
        frame
    }

    #[test]
    fn the_cpu_flips_move_pixels_like_opencv_flip() {
        // 3 wide, 2 tall: [[0, 1, 2], [3, 4, 5]]
        let f = rgba(3, 2);
        let reds = |f: &Frame| f.data.chunks(4).map(|p| p[0]).collect::<Vec<_>>();
        assert_eq!(reds(&through(&View { flip_h: true, ..View::default() }, f.clone())), [2, 1, 0, 5, 4, 3]);
        assert_eq!(reds(&through(&View { flip_v: true, ..View::default() }, f.clone())), [3, 4, 5, 0, 1, 2]);
        assert_eq!(reds(&through(&View { rotation: Some(180), ..View::default() }, f.clone())), [5, 4, 3, 2, 1, 0]);
        assert_eq!(reds(&through(&View { rotation: Some(180), flip_h: true, ..View::default() }, f.clone())), [3, 4, 5, 0, 1, 2], "a half turn and a mirror is a flip");
    }

    #[test]
    fn a_flipped_yuv_picture_flips_each_plane() {
        let mut data: Vec<u8> = (0..4 * 2).collect();
        data.extend([100, 101]);
        data.extend([200, 201]);
        let f = through(&View { flip_h: true, ..View::default() }, Frame::new(4, 2, PixelFormat::I420, data));
        let (y, u, v) = f.i420_planes().unwrap();
        assert_eq!(y, [3, 2, 1, 0, 7, 6, 5, 4]);
        assert_eq!((u, v), (&[101, 100][..], &[201, 200][..]));
        let nv = through(&View { flip_h: true, flip_v: true, ..View::default() }, Frame::new(4, 2, PixelFormat::Nv12, vec![0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 13]));
        assert_eq!(*nv.data, vec![7, 6, 5, 4, 3, 2, 1, 0, 12, 13, 10, 11], "a pair of chroma bytes stays a pair");
    }

    #[test]
    fn a_quarter_turn_is_a_copy_first_and_a_resampling_for_the_rest() {
        let s = steps(&View { rotation: Some(90), ..View::default() });
        assert!(s.len() == 1 && matches!(s[0], FrameStep::Cpu(_)));
        let s = steps(&View { rotation: Some(270), flip_h: true, ..View::default() });
        assert!(s.len() == 2 && s.iter().all(|step| matches!(step, FrameStep::Cpu(_))));
    }

    /// `w` x `h` where the red is `40 * x` and the green `40 * y`.
    fn ramp(w: u32, h: u32) -> Frame {
        Frame::new(w, h, PixelFormat::Rgba, (0..w * h).flat_map(|i| [(i % w * 40) as u8, (i / w * 40) as u8, 7, 255]).collect())
    }

    fn channel(f: &Frame, c: usize) -> Vec<u8> {
        f.data.chunks(4).map(|p| p[c]).collect()
    }

    fn near(got: &[u8], want: &[u8]) {
        assert!(got.len() == want.len() && got.iter().zip(want).all(|(&a, &b)| a.abs_diff(b) <= 1), "{got:?} is not {want:?}");
    }

    #[test]
    fn a_zoom_takes_the_middle_half_at_2x_and_blends_between_pixels() {
        // The crop is x .25 to .75, so the output's first pixel centre is at 0.75 of the source's pixels: 40 * 0.75 = 30
        let out = through(&View { zoom: 2.0, ..View::default() }, ramp(4, 4));
        near(&channel(&out, 0)[..4], &[30, 50, 70, 90]);
        near(&channel(&out, 1).iter().step_by(4).copied().collect::<Vec<_>>(), &[30, 50, 70, 90]);
        assert_eq!((out.width, out.height), (4, 4), "the picture keeps its size");
        assert!(channel(&out, 3).iter().all(|&a| a == 255));
    }

    #[test]
    fn a_panned_zoom_repeats_the_edge_and_a_flip_turns_the_crop_over() {
        // Panned fully right, the window is x .5 to 1: first centre at 1.75 of the source pixels, the last one past the edge
        let out = through(&View { zoom: 2.0, pan_x: 1.0, ..View::default() }, ramp(4, 4));
        near(&channel(&out, 0)[..4], &[70, 90, 110, 120]);
        let flipped = through(&View { zoom: 2.0, flip_h: true, ..View::default() }, ramp(4, 4));
        near(&channel(&flipped, 0)[..4], &[90, 70, 50, 30]);
    }

    #[test]
    fn a_stretch_leaves_black_where_the_picture_does_not_reach() {
        // Half as wide: the outer pixels are bare, the inner two sit at 0.5 and 2.5 of the source's pixels
        let out = through(&View { stretch_x: 0.5, ..View::default() }, ramp(4, 2));
        near(&channel(&out, 0)[..4], &[0, 20, 100, 0]);
        near(&channel(&out, 2)[..4], &[0, 7, 7, 0]);
        assert!(channel(&out, 3).iter().all(|&a| a == 255), "black is opaque");
        // Wider than the picture: it fills the whole frame
        let wide = through(&View { stretch_x: 2.0, ..View::default() }, ramp(4, 2));
        assert!(channel(&wide, 2)[..4].iter().all(|&b| b == 7));
    }

    #[test]
    fn a_zoomed_yuv_picture_keeps_its_colour_and_format() {
        // Y 180, U 100, V 160 all over stays that colour through a zoom and a stretch
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let (w, h) = (64u32, 48u32);
            let c = (w * h / 4) as usize;
            let chroma = if format == PixelFormat::I420 { [vec![100; c], vec![160; c]].concat() } else { [100u8, 160].repeat(c) };
            let f = Frame::new(w, h, format, [vec![180; (w * h) as usize], chroma].concat());
            let out = through(&View { zoom: 1.5, pan_x: 0.3, stretch_y: 1.1, ..View::default() }, f.clone());
            assert_eq!((out.width, out.height, out.format, out.data.len()), (w, h, format, f.data.len()));
            let diff = out.data.iter().zip(f.data.iter()).map(|(&a, &b)| a.abs_diff(b)).max().unwrap();
            assert!(diff <= 3, "{format:?} drifted by {diff}");
        }
    }

    #[test]
    fn a_quarter_turn_with_a_zoom_crops_the_picture_as_it_was_before_the_turn() {
        // 4 wide, 2 tall turned clockwise is 2 wide, 4 tall: the red climbs down the rows, and the green is 40 then 0 across them
        let plain = through(&View { rotation: Some(90), ..View::default() }, ramp(4, 2));
        assert_eq!(channel(&plain, 0), [0, 0, 40, 40, 80, 80, 120, 120]);
        let zoomed = through(&View { rotation: Some(90), zoom: 2.0, ..View::default() }, ramp(4, 2));
        assert_eq!((zoomed.width, zoomed.height), (2, 4));
        near(&channel(&zoomed, 0), &[30, 30, 50, 50, 70, 70, 90, 90]);
        near(&channel(&zoomed, 1), &[30, 10, 30, 10, 30, 10, 30, 10]);
    }

    /// Run by hand for the cost of a 1080p frame: cargo test --release -p telescope-core view_cost -- --ignored --nocapture
    #[test]
    #[ignore]
    fn view_cost() {
        for (name, view) in [("zoom 2x", View { zoom: 2.0, pan_x: 0.3, ..View::default() }), ("stretch", View { stretch_x: 0.8, ..View::default() }),
                             ("zoom and stretch", View { zoom: 1.7, stretch_y: 1.1, flip_h: true, ..View::default() })] {
            for format in [PixelFormat::I420, PixelFormat::Rgba] {
                let data = (0..format.size(1920, 1080)).map(|i| (i * 31 % 251) as u8).collect();
                let mut f = Frame::new(1920, 1080, format, data);
                let mut chain = steps(&view);
                let FrameStep::Cpu(step) = &mut chain[0] else { panic!("cpu") };
                for _ in 0..5 {
                    step.process(&mut f, &NoCtx);
                }
                let t = std::time::Instant::now();
                for _ in 0..60 {
                    step.process(&mut f, &NoCtx);
                }
                println!("{name} {format:?}: {:.2} ms a frame", t.elapsed().as_secs_f64() * 1000.0 / 60.0);
            }
        }
    }

    #[test]
    fn quarter_turns_match_opencv() {
        // 3 wide, 2 tall: [[0, 1, 2], [3, 4, 5]]; cv2.ROTATE_90_CLOCKWISE gives [[3, 0], [4, 1], [5, 2]]
        let f = rgba(3, 2);
        let cw = turn_frame(&f, 1);
        assert_eq!((cw.width, cw.height), (2, 3));
        assert_eq!(cw.data.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [3, 0, 4, 1, 5, 2]);
        // cv2.ROTATE_90_COUNTERCLOCKWISE gives [[2, 5], [1, 4], [0, 3]]
        let ccw = turn_frame(&f, 3);
        assert_eq!(ccw.data.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [2, 5, 1, 4, 0, 3]);
        assert_eq!(turn_frame(&turn_frame(&f, 1), 3).data, f.data);
    }

    #[test]
    fn a_turned_yuv_picture_turns_each_plane() {
        let mut data: Vec<u8> = (0..4 * 2).collect();
        data.extend([100, 101]);
        data.extend([200, 201]);
        let f = Frame::new(4, 2, PixelFormat::I420, data);
        let t = turn_frame(&f, 1);
        assert_eq!((t.width, t.height), (2, 4));
        let (y, u, v) = t.i420_planes().unwrap();
        assert_eq!(y, [4, 0, 5, 1, 6, 2, 7, 3]);
        assert_eq!((u, v), (&[100, 101][..], &[200, 201][..]));
        let nv = Frame::new(2, 2, PixelFormat::Nv12, vec![1, 2, 3, 4, 9, 8]);
        assert_eq!(*turn_frame(&nv, 1).data, vec![3, 1, 4, 2, 9, 8]);
    }

    #[test]
    fn the_resampling_is_told_the_window_the_zoom_crops() {
        let pass = Pass::new(&View { zoom: 2.0, pan_x: 1.0, pan_y: -1.0, ..View::default() }, 0, true, false);
        assert_eq!((pass.x0, pass.y0, pass.crop_w), (0.5, 0.0, 0.5));
        let pass = Pass::new(&View { rotation: Some(90), zoom: 2.0, ..View::default() }, 1, true, false);
        assert_eq!((pass.turn, pass.done), (1, 1));
    }

    #[test]
    fn a_step_keeps_the_view_it_was_made_with() {
        let mut view = View { flip_h: true, zoom: 2.0, ..View::default() };
        let step = steps(&view);
        view.flip_h = false;
        view.rotation = Some(90);
        view.zoom = 1.0;
        assert!(!view.flip_h && view.rotation.is_some() && view.zoom == 1.0);
        let FrameStep::Cpu(mut cpu) = step.into_iter().next().unwrap() else { panic!() };
        let mut f = ramp(4, 4);
        cpu.process(&mut f, &NoCtx);
        near(&channel(&f, 0)[..4], &[90, 70, 50, 30]);
    }
}
