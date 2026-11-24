//! Background's blur on the graphics card against the CPU way, through the app's own frame pipeline.

use super::*;
use crate::chroma_key::ChromaKeyPlugin;
use std::cell::{Cell, RefCell};
use telescope_core::image::rgb_to_420;
use telescope_core::balance::{Balance, Place};
use telescope_core::media::{FramePipeline, StagedStep};
use telescope_media::pipeline::Pipeline;
use telescope_media::transfers;

/// A person mask the vision service would give: the same Arc for as long as the mask is the same, and at a smaller size on request.
struct Ctx {
    mask: Option<Arc<Vec<u8>>>,
    small: RefCell<Option<((usize, usize), Arc<Vec<u8>>)>>,
    /// A new mask Arc on every ask, as when the vision service has a new answer for every frame.
    fresh: Cell<bool>,
}

impl Ctx {
    fn new(mask: Option<Vec<u8>>) -> Ctx {
        Ctx { mask: mask.map(Arc::new), small: RefCell::new(None), fresh: Cell::new(false) }
    }
}

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.mask.clone()
    }
    fn person_mask_at(&self, frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        let mask = self.mask.as_ref()?;
        let mut kept = self.small.borrow_mut();
        if self.fresh.get() || kept.as_ref().is_none_or(|(size, _)| *size != (w, h)) {
            *kept = Some(((w, h), Arc::new(telescope_core::image::resize_area(mask, frame.width as usize, frame.height as usize, 1, w, h))));
        }
        kept.as_ref().map(|(_, m)| m.clone())
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// A person as a mask: a head and shoulders with a soft edge a few pixels wide.
fn person(w: usize, h: usize) -> Vec<u8> {
    let (cx, cy, r) = (w as f32 * 0.5, h as f32 * 0.42, h as f32 * 0.17);
    (0..w * h).map(|i| {
        let (x, y) = ((i % w) as f32, (i / w) as f32);
        let head = ((x - cx) / (r * 0.8)).powi(2) + ((y - cy) / r).powi(2);
        let body = ((x - cx) / (r * 2.6)).powi(2) + ((y - cy - r * 2.3) / (r * 1.8)).powi(2);
        let f = head.min(body);
        (255.0 * ((1.12 - f) / 0.24).clamp(0.0, 1.0)) as u8
    }).collect()
}

/// A room with colour in it (blocks, a gradient, noise) and a bright orange person where the mask has one, as an I420 frame.
fn room(w: usize, h: usize, mask: &[u8]) -> Frame {
    let rgb = room_rgb(w, h, mask);
    let mut data = vec![0u8; PixelFormat::I420.size(w as u32, h as u32)];
    rgb_to_420(&rgb, w, h, &mut data, false);
    Frame::new(w as u32, h as u32, PixelFormat::I420, data)
}

/// The same room as red, green and blue bytes.
fn room_rgb(w: usize, h: usize, mask: &[u8]) -> Vec<u8> {
    let mut rgb = vec![0u8; w * h * 3];
    let mut n = 9u32;
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (n >> 29) as f32 - 3.0;
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let blocks = ((x / 160 + y / 120) % 3) as f32;
            let room = [60.0 + 90.0 * u + 30.0 * blocks, 70.0 + 80.0 * v, 120.0 + 60.0 * (u * 9.0).sin().abs() - 25.0 * blocks];
            let skin = [235.0, 120.0 + 30.0 * v, 40.0];
            let m = mask[y * w + x] as f32 / 255.0;
            for c in 0..3 {
                rgb[(y * w + x) * 3 + c] = (room[c] * (1.0 - m) + skin[c] * m + noise).clamp(0.0, 255.0) as u8;
            }
        }
    }
    rgb
}

fn plugin(strength: i64, edges: i64) -> BackgroundPlugin {
    let mut p = BackgroundPlugin::new();
    p.set_config(&json!({"mode": "blur", "blur": strength, "edges": edges})).unwrap();
    p
}

/// A card the frames really go to: Windows' software renderer (WARP) is found as a card, but the steps stay on the CPU there.
fn real_card() -> bool {
    let here = telescope_media::gpu::Gpu::shared().is_some_and(|gpu| !gpu.adapter_name.contains("Basic Render"));
    if !here {
        eprintln!("no graphics card here: the card checks are skipped");
    }
    here
}

/// A pipeline with a balance of its own, as the card's place, so tests that move the step don't move each other's
fn own_pipeline(balance: Balance) -> Pipeline {
    Pipeline::with_balance(Arc::new(balance))
}

fn pipeline_with(step: FrameStep) -> Pipeline {
    let mut pipeline = own_pipeline(Balance::pinned_to(Place::Card));
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Background, step }]);
    pipeline
}

/// The frame through the plugin's step on the card, and through its CPU half alone: (card, cpu).
fn both(p: &mut BackgroundPlugin, frame: &Frame, ctx: &Ctx) -> (Frame, Frame) {
    let Some(step @ FrameStep::Either(..)) = p.frame_step() else { panic!("Blur runs either way") };
    let mut pipeline = pipeline_with(step);
    let before = transfers::totals();
    let card = pipeline.process(frame.clone(), ctx);
    assert!(transfers::totals().up > before.up, "the frame didn't go to the card");
    let Some(FrameStep::Either(_, mut step)) = p.frame_step() else { panic!("a step") };
    let mut cpu = frame.clone();
    step.process(&mut cpu, ctx);
    (card, cpu)
}

/// How far the card's picture is from the CPU's: the biggest difference and the mean over every byte, and the biggest one more than `margin` pixels (at the picture's own size) away from anywhere the mask changes.
fn gap(card: &Frame, cpu: &Frame, mask: &[u8], margin: usize) -> (i32, f64, i32) {
    let (w, h) = (card.width as usize, card.height as usize);
    // a pixel is near an edge when the mask isn't the same all around it
    let near = |x: usize, y: usize| {
        let (x0, x1, y0, y1) = (x.saturating_sub(margin), (x + margin).min(w - 1), y.saturating_sub(margin), (y + margin).min(h - 1));
        let m = mask[y * w + x];
        [(x0, y0), (x1, y0), (x0, y1), (x1, y1), (x0, y), (x1, y), (x, y0), (x, y1)].iter().any(|&(nx, ny)| mask[ny * w + nx] != m)
    };
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = [(0, w, h, 1usize), (w * h, cw, ch, 2), (w * h + cw * ch, cw, ch, 2)];
    let (mut worst, mut far, mut sum) = (0, 0, 0u64);
    for (start, pw, ph, scale) in planes {
        for y in 0..ph {
            for x in 0..pw {
                let d = (card.data[start + y * pw + x] as i32 - cpu.data[start + y * pw + x] as i32).abs();
                worst = worst.max(d);
                sum += d as u64;
                if !near((x * scale).min(w - 1), (y * scale).min(h - 1)) {
                    far = far.max(d);
                }
            }
        }
    }
    (worst, sum as f64 / card.data.len() as f64, far)
}

#[test]
fn the_card_gives_the_picture_the_cpu_does_at_every_strength_and_edge() {
    if !real_card() {
        return;
    }
    let (w, h) = (1280, 720);
    let mask = person(w, h);
    let frame = room(w, h, &mask);
    let ctx = Ctx::new(Some(mask.clone()));
    let mut worst_of_all = (0, 0.0f64, 0);
    for strength in [1, 6, 10] {
        for edges in [0, 3] {
            let (card, cpu) = both(&mut plugin(strength, edges), &frame, &ctx);
            assert_eq!((card.width, card.height, card.format), (cpu.width, cpu.height, cpu.format));
            let (max, mean, far) = gap(&card, &cpu, &mask, 24);
            eprintln!("blur {strength} edges {edges}: card vs CPU max {max} levels, {far} away from the mask's edge, mean {mean:.3}");
            assert!(far <= 4 && mean <= 1.0 && max <= 6, "blur {strength} edges {edges}: max {max}, far {far}, mean {mean:.3}");
            worst_of_all = (worst_of_all.0.max(max), worst_of_all.1.max(mean), worst_of_all.2.max(far));
            // where the mask says person the person stays, as sharp as they were
            let mut worst_person = 0;
            for y in (40..h - 40).step_by(3) {
                for x in (40..w - 40).step_by(3) {
                    let near_edge = (y.saturating_sub(24)..(y + 24).min(h)).step_by(8).any(|ny| (x.saturating_sub(24)..(x + 24).min(w)).step_by(8).any(|nx| mask[ny * w + nx] != 255));
                    if mask[y * w + x] == 255 && !near_edge {
                        worst_person = worst_person.max((card.data[y * w + x] as i32 - frame.data[y * w + x] as i32).abs());
                    }
                }
            }
            assert!(worst_person <= 2, "blur {strength} edges {edges}: the person's luma moved by {worst_person}");
        }
    }
    eprintln!("worst of all: max {} levels, {} away from edges, mean {:.3}", worst_of_all.0, worst_of_all.2, worst_of_all.1);
}

#[test]
fn a_frame_with_no_mask_gets_the_whole_blur_on_the_card_as_on_the_cpu() {
    if !real_card() {
        return;
    }
    let (w, h) = (1280, 720);
    let mask = person(w, h);
    let frame = room(w, h, &mask);
    let ctx = Ctx::new(None);
    for strength in [1, 10] {
        let (card, cpu) = both(&mut plugin(strength, 3), &frame, &ctx);
        let (max, mean, _) = gap(&card, &cpu, &vec![0; w * h], 24);
        eprintln!("no mask, blur {strength}: card vs CPU max {max} levels, mean {mean:.3}");
        assert!(max <= 4 && mean <= 1.0, "no mask, blur {strength}: max {max}, mean {mean:.3}");
        // and it is the blur: nothing of the sharp picture is left
        let moved = (0..w * h).step_by(7).map(|i| (card.data[i] as i32 - frame.data[i] as i32).abs()).max().unwrap();
        assert!(moved > 10, "the picture is still sharp ({moved})");
    }
}

#[test]
fn the_room_next_to_the_person_stays_the_colour_of_the_room() {
    if !real_card() {
        return;
    }
    let (w, h) = (640, 360);
    let mask = person(w, h);
    // a plain grey room, so any colour in it came from the person
    let mut rgb = vec![110u8; w * h * 3];
    for (i, m) in mask.iter().enumerate() {
        if *m == 255 {
            rgb[i * 3..i * 3 + 3].copy_from_slice(&[240, 30, 200]);
        }
    }
    let mut data = vec![0u8; PixelFormat::I420.size(w as u32, h as u32)];
    rgb_to_420(&rgb, w, h, &mut data, false);
    let frame = Frame::new(w as u32, h as u32, PixelFormat::I420, data);
    let ctx = Ctx::new(Some(mask.clone()));
    let (card, cpu) = both(&mut plugin(10, 3), &frame, &ctx);
    let (cw, ch) = (w / 2, h / 2);
    let (u0, v0) = (w * h, w * h + cw * ch);
    let (mut worst_card, mut worst_cpu) = (0, 0);
    for y in 0..ch {
        for x in 0..cw {
            // room only: the mask is 0 for 20 pixels around
            let (fx, fy) = (x * 2, y * 2);
            let clear = (fy.saturating_sub(20)..(fy + 20).min(h)).step_by(4).all(|ny| (fx.saturating_sub(20)..(fx + 20).min(w)).step_by(4).all(|nx| mask[ny * w + nx] == 0));
            if clear {
                for (frame, worst) in [(&card, &mut worst_card), (&cpu, &mut worst_cpu)] {
                    *worst = (*worst).max((frame.data[u0 + y * cw + x] as i32 - 128).abs().max((frame.data[v0 + y * cw + x] as i32 - 128).abs()));
                }
            }
        }
    }
    eprintln!("room chroma off grey by {worst_card} on the card, {worst_cpu} on the CPU");
    assert!(worst_card <= worst_cpu + 2, "the person's colour leaked into the room: {worst_card} against {worst_cpu} on the CPU");
}

#[test]
fn the_balance_can_still_move_the_step_to_the_cpu_where_it_gives_the_cpu_picture() {
    let (w, h) = (640, 360);
    let mask = person(w, h);
    let frame = room(w, h, &mask);
    let ctx = Ctx::new(Some(mask));
    let mut p = plugin(6, 3);
    let Some(step @ FrameStep::Either(..)) = p.frame_step() else { panic!("Blur runs either way") };
    // a card that keeps frames waiting for long moves the step off it
    let balance = Arc::new(Balance::free());
    let mut pipeline = Pipeline::with_balance(balance.clone());
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Background, step }]);
    (0..8).for_each(|_| pipeline.lane().note_wait(50_000_000));
    assert_eq!(pipeline.lane().place(), Place::Cpu);
    let out = pipeline.process(frame.clone(), &ctx);
    // this pipeline's own count, so a frame another test sends to the card doesn't show here
    assert_eq!(pipeline.last_transfers(), (0, 0), "nothing went up or came down");
    let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
    let mut want = frame.clone();
    cpu.process(&mut want, &ctx);
    assert_eq!(out.data, want.data, "the CPU half gave the CPU picture");
}

#[test]
fn the_blur_on_the_card_still_gives_a_stream_its_cut_out() {
    if !real_card() {
        return;
    }
    let (w, h) = (640, 360);
    let mask = person(w, h);
    let frame = room(w, h, &mask);
    let ctx = Ctx::new(Some(mask));
    let mut p = plugin(6, 3);
    p.wanted.set("phone", true);
    let (card, cpu) = both(&mut p, &frame, &ctx);
    let (on_card, on_cpu) = (card.alpha().expect("the card's frame has the cut-out"), cpu.alpha().expect("the CPU's frame has it"));
    assert_eq!(on_card.data, on_cpu.data);
    assert!(on_card.data.contains(&255) && on_card.data.contains(&0), "a person and a room");
    // a stream that doesn't ask gets none
    let mut p = plugin(6, 3);
    assert!(both(&mut p, &frame, &ctx).0.alpha().is_none());
}

/// A picture with bands, a gradient and a size of its own (not the frame's), as a chosen file would be.
fn backdrop_picture() -> Arc<Rgb> {
    let (w, h) = (900, 700);
    let mut data = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let band = if (x / 90) % 2 == 0 { 40.0 } else { 0.0 };
            data.extend([(x * 255 / w) as f32 + band, (y * 255 / h) as f32, 200.0 - (x * 120 / w) as f32 + band].map(|v| v.clamp(0.0, 255.0) as u8));
        }
    }
    Arc::new(Rgb { w, h, data })
}

fn picture_plugin(edges: i64) -> BackgroundPlugin {
    let mut p = BackgroundPlugin::new();
    p.set_config(&json!({"mode": "picture", "edges": edges})).unwrap();
    p.picture = Some(backdrop_picture());
    p
}

#[test]
fn picture_mode_runs_either_way_when_it_has_a_picture_and_stays_on_the_cpu_when_it_has_none() {
    let mut p = BackgroundPlugin::new();
    p.set_config(&json!({"mode": "picture", "blur": 6})).unwrap();
    assert!(matches!(p.frame_step(), Some(FrameStep::Cpu(_))), "no picture, nothing for the card to lay over");
    p.picture = Some(backdrop_picture());
    assert!(matches!(p.frame_step(), Some(FrameStep::Either(..))));
    p.set_config(&json!({"mode": "blur", "blur": 6})).unwrap();
    assert!(matches!(p.frame_step(), Some(FrameStep::Either(..))));
}

#[test]
fn the_card_gives_the_picture_the_cpu_does_in_picture_mode() {
    if !real_card() {
        return;
    }
    let (w, h) = (1280, 720);
    let mask = person(w, h);
    let frame = room(w, h, &mask);
    let ctx = Ctx::new(Some(mask.clone()));
    for edges in [0, 3] {
        let (card, cpu) = both(&mut picture_plugin(edges), &frame, &ctx);
        let (max, mean, far) = gap(&card, &cpu, &mask, 24);
        eprintln!("picture, edges {edges}: card vs CPU max {max} levels, {far} away from the mask's edge, mean {mean:.3}");
        assert!(far <= 4 && mean <= 1.0 && max <= 6, "picture edges {edges}: max {max}, far {far}, mean {mean:.3}");
        let mut worst_person = 0;
        for y in (40..h - 40).step_by(3) {
            for x in (40..w - 40).step_by(3) {
                let near_edge = (y.saturating_sub(24)..(y + 24).min(h)).step_by(8).any(|ny| (x.saturating_sub(24)..(x + 24).min(w)).step_by(8).any(|nx| mask[ny * w + nx] != 255));
                if mask[y * w + x] == 255 && !near_edge {
                    worst_person = worst_person.max((card.data[y * w + x] as i32 - frame.data[y * w + x] as i32).abs());
                }
            }
        }
        assert!(worst_person <= 2, "picture edges {edges}: the person's luma moved by {worst_person}");
    }
}

#[test]
fn a_frame_with_no_mask_is_all_picture_on_the_card_as_on_the_cpu() {
    if !real_card() {
        return;
    }
    let (w, h) = (1280, 720);
    let frame = room(w, h, &person(w, h));
    let (card, cpu) = both(&mut picture_plugin(3), &frame, &Ctx::new(None));
    let (max, mean, _) = gap(&card, &cpu, &vec![0; w * h], 24);
    eprintln!("picture, no mask: card vs CPU max {max} levels, mean {mean:.3}");
    assert!(max <= 4 && mean <= 1.0, "no mask: max {max}, mean {mean:.3}");
    assert!(card.data.iter().step_by(11).any(|&v| v != 0), "not a dark frame");
}

#[test]
fn picture_mode_on_rgba_and_odd_sized_frames_matches_the_cpu() {
    if !real_card() {
        return;
    }
    for (w, h) in [(1280usize, 720usize), (1281, 721)] {
        let mask = person(w, h);
        let mut rgba = Frame::new(w as u32, h as u32, PixelFormat::Rgba, vec![255; w * h * 4]);
        for (o, p) in rgba.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(room_rgb(w, h, &mask).as_chunks::<3>().0) {
            o[..3].copy_from_slice(p);
        }
        let ctx = Ctx::new(Some(mask.clone()));
        let (card, cpu) = both(&mut picture_plugin(3), &rgba, &ctx);
        let worst = card.data.iter().zip(cpu.data.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        let mean = card.data.iter().zip(cpu.data.iter()).map(|(a, b)| (*a as i64 - *b as i64).abs()).sum::<i64>() as f64 / card.data.len() as f64;
        eprintln!("picture rgba {w}x{h}: max {worst}, mean {mean:.3}");
        assert!(worst <= 12 && mean <= 1.0, "{w}x{h}: max {worst}, mean {mean:.3}");
    }
}

/// The mean of the luma plane, which a dark frame would pull down.
fn mean_luma(f: &Frame) -> f64 {
    let n = (f.width * f.height) as usize;
    f.data[..n].iter().step_by(5).map(|&v| v as f64).sum::<f64>() / n.div_ceil(5) as f64
}

#[test]
fn picture_mode_never_flashes_dark_through_masks_settings_and_the_balance_moving_it() {
    if !real_card() {
        return;
    }
    let (w, h) = (640, 360);
    let mask = person(w, h);
    let shifted: Vec<u8> = (0..w * h).map(|i| mask[(i + 37) % (w * h)]).collect();
    let frame = room(w, h, &mask);
    let balance = Arc::new(Balance::free());
    let mut pipeline = Pipeline::with_balance(balance.clone());
    let step_for = |edges: i64| {
        let mut p = picture_plugin(edges);
        p.frame_step().unwrap()
    };
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Background, step: step_for(3) }]);
    let mut means = Vec::new();
    let mut run = |pipeline: &mut Pipeline, ctx: &Ctx, n: usize| (0..n).for_each(|_| means.push(mean_luma(&pipeline.process(frame.clone(), ctx))));
    // nobody found yet, a person appears, they move (a new mask every frame), the edge setting changes, the card gets busy and the step moves to the CPU
    run(&mut pipeline, &Ctx::new(None), 3);
    run(&mut pipeline, &Ctx::new(Some(mask.clone())), 3);
    run(&mut pipeline, &Ctx::new(Some(shifted.clone())), 3);
    run(&mut pipeline, &Ctx::new(Some(mask.clone())), 2);
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Background, step: step_for(8) }]);
    run(&mut pipeline, &Ctx::new(Some(mask.clone())), 3);
    (0..8).for_each(|_| pipeline.lane().note_wait(50_000_000));
    assert_eq!(pipeline.lane().place(), Place::Cpu);
    run(&mut pipeline, &Ctx::new(Some(mask.clone())), 3);
    run(&mut pipeline, &Ctx::new(None), 2);
    let (lo, hi) = means.iter().fold((f64::MAX, 0.0f64), |(l, h), &m| (l.min(m), h.max(m)));
    eprintln!("picture mode luma across the run: {lo:.1} to {hi:.1}, steps between frames at most {:.1}", means.windows(2).map(|p| (p[0] - p[1]).abs()).fold(0.0, f64::max));
    assert!(lo > 0.7 * hi && lo > 30.0, "a frame went dark: {means:?}");
    assert!(means.windows(2).all(|p| (p[0] - p[1]).abs() < 0.2 * hi), "the picture jumped between two frames: {means:?}");
}

/// Changes the picture after the others, as a step on the CPU does.
struct Touches;

impl CpuStep for Touches {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        frame.data_mut()[0] = frame.data[0].wrapping_add(1);
    }
}

#[test]
fn the_card_blur_next_to_a_key_and_a_cpu_step_costs_one_trip_each_way_and_a_mask_goes_up_once_it_changes() {
    if !real_card() {
        return;
    }
    let (w, h) = (1280, 720);
    let mask = person(w, h);
    let frame = room(w, h, &mask);
    let per_frame = |blur: FrameStep, ctx: &Ctx, frames: u64, with_key: bool| {
        let mut pipeline = own_pipeline(Balance::pinned_to(Place::Card));
        let mut key = ChromaKeyPlugin::new();
        key.set_config(&json!({"on": true, "colour": [0, 255, 0]})).unwrap();
        let mut steps = vec![StagedStep { owner: NAME.into(), stage: FrameStage::Background, step: blur }];
        if with_key {
            steps.push(StagedStep { owner: "key".into(), stage: FrameStage::Background, step: key.frame_step().expect("a key") });
            steps.push(StagedStep { owner: "cpu".into(), stage: FrameStage::Effects, step: FrameStep::Cpu(Box::new(Touches)) });
        }
        pipeline.set_steps(steps);
        pipeline.process(frame.clone(), ctx);
        let before = transfers::totals();
        for _ in 0..frames {
            pipeline.process(frame.clone(), ctx);
        }
        let after = transfers::totals();
        ((after.up - before.up) as f64 / frames as f64, (after.down - before.down) as f64 / frames as f64, after.mask_up - before.mask_up)
    };
    let ctx = Ctx::new(Some(mask));
    let mut p = plugin(6, 3);
    for (with_key, what) in [(false, "Background alone"), (true, "Background, a key and a CPU step after")] {
        let Some(FrameStep::Either(_, step)) = p.frame_step() else { panic!("Blur runs either way") };
        let (up_cpu, down_cpu, _) = per_frame(FrameStep::Cpu(step), &ctx, 10, with_key);
        let Some(either) = p.frame_step() else { panic!("a step") };
        let (up_card, down_card, masks) = per_frame(either, &ctx, 10, with_key);
        eprintln!("{what}: Background on the CPU {up_cpu} up {down_cpu} down a frame, on the card {up_card} up {down_card} down, {masks} mask pictures up over 10 frames with the same mask");
        assert_eq!((up_card, down_card), (1.0, 1.0), "{what}: the whole run is one trip each way");
        assert_eq!(masks, 0, "the same mask isn't sent again");
    }
    // the cutout and the share are two pictures, up the first frame of a mask and not again until the mask changes
    ctx.fresh.set(true);
    let Some(either) = p.frame_step() else { panic!("a step") };
    let (_, _, masks) = per_frame(either, &ctx, 10, true);
    assert_eq!(masks, 20, "a new mask every frame sends both pictures every frame");
}

/// What a frame costs the CPU way and on the card, in ms, including the card's upload and landing. Run with
/// cargo nextest run --release -p telescope-effects --run-ignored only background_card_tests::what_the_blur_costs
#[test]
#[ignore = "a measurement, needs a graphics card and a release build"]
fn what_the_blur_costs() {
    assert!(real_card(), "a graphics card");
    println!("on {}", telescope_media::gpu::Gpu::shared().unwrap().adapter_name);
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let mask = person(w, h);
        let frame = room(w, h, &mask);
        for (strength, edges) in [(6, 3), (10, 3)] {
            for fresh in [false, true] {
                let ctx = Ctx::new(Some(mask.clone()));
                ctx.fresh.set(fresh);
                let mut p = plugin(strength, edges);
                let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
                let mut cpu_ms = Vec::new();
                for i in 0..40 {
                    let mut f = frame.clone();
                    let began = std::time::Instant::now();
                    cpu.process(&mut f, &ctx);
                    if i >= 8 {
                        cpu_ms.push(began.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                let Some(either) = p.frame_step() else { panic!("a step") };
                let mut pipeline = pipeline_with(either);
                let mut card_ms = Vec::new();
                for i in 0..40 {
                    let began = std::time::Instant::now();
                    pipeline.process(frame.clone(), &ctx);
                    if i >= 8 {
                        card_ms.push(began.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                let median = |v: &mut Vec<f64>| {
                    v.sort_by(|a, b| a.total_cmp(b));
                    v[v.len() / 2]
                };
                println!("{w}x{h} blur {strength} edges {edges}, {} mask: CPU {:.2} ms a frame, card {:.2} ms (with upload and landing)",
                         if fresh { "new" } else { "same" }, median(&mut cpu_ms), median(&mut card_ms));
            }
        }
    }
}

/// The room as an RGBA or BGRA frame.
fn room_in(format: PixelFormat, w: usize, h: usize, mask: &[u8]) -> Frame {
    let rgb = room_rgb(w, h, mask);
    let data = rgb.as_chunks::<3>().0.iter().flat_map(|p| if format == PixelFormat::Rgba { [p[0], p[1], p[2], 255] } else { [p[2], p[1], p[0], 255] }).collect();
    Frame::new(w as u32, h as u32, format, data)
}

#[test]
fn rgba_and_bgra_frames_use_the_full_size_mask_where_the_cpu_does() {
    if !real_card() {
        return;
    }
    let (w, h) = (640, 360);
    // a person, and a few hairs a pixel or two wide that a quarter-size mask would lose
    let mut mask = person(w, h);
    for y in 20..140 {
        for x in [100, 101, 230, 231, 232] {
            mask[y * w + x] = 255;
        }
    }
    let ctx = Ctx::new(Some(mask.clone()));
    for format in [PixelFormat::Rgba, PixelFormat::Bgra] {
        let frame = room_in(format, w, h, &mask);
        let (card, cpu) = both(&mut plugin(6, 0), &frame, &ctx);
        let worst = card.data.iter().zip(cpu.data.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        let mean = card.data.iter().zip(cpu.data.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs() as f64).sum::<f64>() / card.data.len() as f64;
        // the hair is the room's picture as it was, on the CPU and on the card
        let hair = (60 * w + 101) * 4;
        let moved: i32 = (0..3).map(|c| (card.data[hair + c] as i32 - frame.data[hair + c] as i32).abs()).max().unwrap();
        eprintln!("{format:?} with no feathering: card vs CPU max {worst}, mean {mean:.3}; the hair moved {moved} on the card");
        assert!(worst <= 8 && mean <= 1.0, "{format:?}: max {worst}, mean {mean:.3}");
        assert!(moved <= 3, "{format:?}: the hair was blurred away ({moved})");
    }
}

/// A frame of two flat colours too saturated for RGB (blue and red, as planes) with a person in YUV samples of their own.
fn two_tone(w: usize, h: usize, mask: &[u8]) -> Frame {
    let (cw, ch) = (w / 2, h / 2);
    let mut data = vec![0u8; PixelFormat::I420.size(w as u32, h as u32)];
    let tone = |x: usize| if x < w / 2 { (90u8, 250u8, 90u8) } else { (170, 20, 240) };
    let person = |x: usize, y: usize| mask[y * w + x] == 255;
    for y in 0..h {
        for x in 0..w {
            data[y * w + x] = if person(x, y) { 205 } else { tone(x).0 };
        }
    }
    for y in 0..ch {
        for x in 0..cw {
            let inside = person(x * 2, y * 2);
            data[w * h + y * cw + x] = if inside { 60 } else { tone(x * 2).1 };
            data[w * h + cw * ch + y * cw + x] = if inside { 200 } else { tone(x * 2).2 };
        }
    }
    Frame::new(w as u32, h as u32, PixelFormat::I420, data)
}

#[test]
fn a_saturated_room_blurs_the_colour_the_cpu_does_and_the_person_keeps_their_own_samples() {
    if !real_card() {
        return;
    }
    let (w, h) = (640, 360);
    let mask = person(w, h);
    let frame = two_tone(w, h, &mask);
    let ctx = Ctx::new(Some(mask.clone()));
    let (card, cpu) = both(&mut plugin(6, 3), &frame, &ctx);
    let (max, mean, far) = gap(&card, &cpu, &mask, 24);
    eprintln!("two saturated tones: card vs CPU max {max} levels, {far} away from the mask's edge, mean {mean:.3}");
    assert!(far <= 3 && mean <= 1.0, "max {max}, far {far}, mean {mean:.3}");
    // where the mask says person, far from its edge, every plane is the frame's own sample as far as the CPU leaves it: no trip through RGB
    let (cw, ch) = (w / 2, h / 2);
    let (mut checked, mut moved_card, mut moved_cpu) = (0, 0, 0);
    for y in (30..h - 30).step_by(2) {
        for x in (30..w - 30).step_by(2) {
            let solid = (y - 12..y + 12).step_by(4).all(|ny| (x - 12..x + 12).step_by(4).all(|nx| mask[ny * w + nx] == 255));
            if solid {
                for i in [y * w + x, w * h + (y / 2) * cw + x / 2, w * h + cw * ch + (y / 2) * cw + x / 2] {
                    assert!((card.data[i] as i32 - cpu.data[i] as i32).abs() <= 1, "at {x},{y}: the card made {} where the CPU made {}", card.data[i], cpu.data[i]);
                    moved_card = moved_card.max((card.data[i] as i32 - frame.data[i] as i32).abs());
                    moved_cpu = moved_cpu.max((cpu.data[i] as i32 - frame.data[i] as i32).abs());
                }
                checked += 1;
            }
        }
    }
    eprintln!("the person's samples moved by up to {moved_card} on the card and {moved_cpu} on the CPU");
    assert!(moved_card <= moved_cpu + 1);
    assert!(checked > 100, "{checked} samples checked");
}
