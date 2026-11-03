//! The pixel work of the window's view (view.apply_zoom, transform_frame and apply_stretch), as frame steps for the View stage.
//!
//! Python crops, flips, turns and then stretches the picture. A shader can't change the picture's size, so a quarter turn is
//! a CPU step first (an exact copy of the planes) and the shader does the rest as if it were given the turned picture. The
//! zoom is a fractional crop that repeats the edge, the stretch leaves black where the picture doesn't reach, both like
//! cv2.warpAffine's linear interpolation does.

use crate::frame::{CpuStep, Frame, FrameContext, FrameStep, PixelFormat, ShaderStep};
use crate::host::View;
use crate::view::window;
use serde_json::json;

const WGSL: &str = "
struct Params {
    x0: f32,
    y0: f32,
    crop_w: f32,
    crop_h: f32,
    flip_h: f32,
    flip_v: f32,
    stretch_x: f32,
    stretch_y: f32,
    turn: u32,
    done: u32,
}

fn unturn(p: vec2<f32>, quarters: u32) -> vec2<f32> {
    if quarters == 1u { return vec2<f32>(p.y, 1.0 - p.x); }
    if quarters == 2u { return vec2<f32>(1.0 - p.x, 1.0 - p.y); }
    if quarters == 3u { return vec2<f32>(1.0 - p.y, p.x); }
    return p;
}

fn turned(p: vec2<f32>, quarters: u32) -> vec2<f32> {
    if quarters == 1u { return vec2<f32>(1.0 - p.y, p.x); }
    if quarters == 2u { return vec2<f32>(1.0 - p.x, 1.0 - p.y); }
    if quarters == 3u { return vec2<f32>(p.y, 1.0 - p.x); }
    return p;
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let s = vec2<f32>(0.5) + (uv - vec2<f32>(0.5)) / vec2<f32>(params.stretch_x, params.stretch_y);
    var p = unturn(s, params.turn);
    if params.flip_h > 0.5 { p.x = 1.0 - p.x; }
    if params.flip_v > 0.5 { p.y = 1.0 - p.y; }
    p = vec2<f32>(params.x0 + p.x * params.crop_w, params.y0 + p.y * params.crop_h);
    p = turned(p, params.done);
    // Past the edge of the stretched picture it fades to black over half a pixel, as a constant border does
    let at = s * frame.size;
    let inside = clamp(at + vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(1.0)) * clamp(frame.size - at + vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(1.0));
    return vec4<f32>(sample(p).rgb * inside.x * inside.y, 1.0);
}
";

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

/// The uniforms of a shader pass for the picture as given (turned by `done` quarters already): `zoom` the crop, flips and half turn of
/// `view`, `stretch` its stretch. Python does these in two resamplings, the stretch of the 8-bit result of the crop, and so does
/// this, so thin lines blur the same amount.
fn uniforms(view: &View, done: u32, zoom: bool, stretch: bool) -> serde_json::Value {
    let (z, pan_x, pan_y) = if zoom { (view.zoom as f64, (view.pan_x as f64).clamp(-1.0, 1.0), (view.pan_y as f64).clamp(-1.0, 1.0)) } else { (1.0, 0.0, 0.0) };
    let ((x0, w), (y0, h)) = (window(z, pan_x), window(z, pan_y));
    let (sx, sy) = if stretch { (view.stretch_x, view.stretch_y) } else { (1.0, 1.0) };
    json!({"x0": x0, "y0": y0, "crop_w": w, "crop_h": h, "flip_h": zoom && view.flip_h, "flip_v": zoom && view.flip_v,
           "stretch_x": sx, "stretch_y": sy, "turn": if zoom { quarters(view.rotation) } else { 0 }, "done": if zoom { done } else { 0 }})
}

/// Whether the crop, flips or a half turn of the view change the picture.
fn crop_needed(view: &View) -> bool {
    view.zoom > 1.0 || view.flip_h || view.flip_v || quarters(view.rotation) == 2
}

fn stretch_needed(view: &View) -> bool {
    (view.stretch_x - 1.0).abs() >= 1e-6 || (view.stretch_y - 1.0).abs() >= 1e-6
}

/// The frame steps of a view, in order: a CPU quarter turn when the picture changes shape, a shader for the crop, flips and half
/// turn, and one for the stretch. No steps for a view that changes nothing. A view that only mirrors, flips or half turns is a CPU
/// copy instead: a busy card (OBS encoding) would hold up every frame for a shader, and a shader step can't move to the CPU.
pub fn steps(view: &View) -> Vec<FrameStep> {
    let q = quarters(view.rotation);
    let mut out: Vec<FrameStep> = Vec::new();
    // Turned first, the rest of the work is the same shader as for any other view, on the turned picture
    let done = if q == 1 || q == 3 { q } else { 0 };
    if done != 0 {
        out.push(FrameStep::Cpu(Box::new(Turn { quarters: done })));
    }
    if crop_needed(view) && done == 0 && view.zoom <= 1.0 && !stretch_needed(view) {
        // A half turn is both flips, and a second flip undoes the first
        let half = quarters(view.rotation) == 2;
        out.push(FrameStep::Cpu(Box::new(Flip { horizontal: view.flip_h != half, vertical: view.flip_v != half })));
    } else if crop_needed(view) {
        out.push(FrameStep::Shader(ShaderStep { wgsl: WGSL.into(), uniforms: uniforms(view, done, true, false), textures: Vec::new() }));
    }
    if stretch_needed(view) {
        out.push(FrameStep::Shader(ShaderStep { wgsl: WGSL.into(), uniforms: uniforms(view, 0, false, true), textures: Vec::new() }));
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
    fn zoom_and_stretch_are_one_shader() {
        for view in [View { zoom: 2.0, ..View::default() }, View { stretch_x: 0.8, ..View::default() }, View { flip_h: true, zoom: 2.0, ..View::default() }] {
            let s = steps(&view);
            assert!(s.len() == 1 && matches!(s[0], FrameStep::Shader(_)), "{view:?}");
        }
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
    fn a_quarter_turn_is_a_copy_first_and_a_shader_only_for_the_rest() {
        let s = steps(&View { rotation: Some(90), ..View::default() });
        assert!(s.len() == 1 && matches!(s[0], FrameStep::Cpu(_)));
        let s = steps(&View { rotation: Some(270), flip_h: true, ..View::default() });
        assert!(matches!(s[0], FrameStep::Cpu(_)) && matches!(s[1], FrameStep::Shader(_)));
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
    fn the_shader_is_told_the_window_the_zoom_crops() {
        let FrameStep::Shader(s) = &steps(&View { zoom: 2.0, pan_x: 1.0, pan_y: -1.0, ..View::default() })[0] else { panic!() };
        assert_eq!((s.uniforms["x0"].as_f64().unwrap(), s.uniforms["y0"].as_f64().unwrap(), s.uniforms["crop_w"].as_f64().unwrap()), (0.5, 0.0, 0.5));
        let FrameStep::Shader(s) = &steps(&View { rotation: Some(90), zoom: 2.0, ..View::default() })[1] else { panic!() };
        assert_eq!((s.uniforms["turn"].as_u64(), s.uniforms["done"].as_u64()), (Some(1), Some(1)));
    }

    #[test]
    fn a_step_keeps_the_view_it_was_made_with() {
        let mut view = View { flip_h: true, zoom: 2.0, ..View::default() };
        let step = steps(&view);
        view.flip_h = false;
        view.rotation = Some(90);
        view.zoom = 1.0;
        assert!(!view.flip_h && view.rotation.is_some() && view.zoom == 1.0);
        let FrameStep::Shader(s) = &step[0] else { panic!() };
        assert_eq!((step.len(), s.uniforms["flip_h"].as_bool(), s.uniforms["crop_w"].as_f64()), (1, Some(true), Some(0.5)));
    }
}
