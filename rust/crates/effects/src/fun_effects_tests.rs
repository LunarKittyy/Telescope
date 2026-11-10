use super::*;
use telescope_core::frame::PixelFormat;
use telescope_core::image::over_rows;

const W: usize = 160;
const H: usize = 90;

/// A room with a bit of texture, so differences from it are real ones.
fn room() -> Rgb {
    let mut r = Rgb::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let b = (40.0 + 80.0 * x as f32 / (W - 1) as f32) as u8;
            let g = (60.0 + 40.0 * y as f32 / (H - 1) as f32) as u8;
            r.set(x, y, [70, g, b]);
        }
    }
    r
}

fn with_person(room: &Rgb, x0: usize, x1: usize, colour: [u8; 3]) -> Rgb {
    let mut f = room.clone();
    for y in 20..80 {
        for x in x0..x1 {
            f.set(x, y, colour);
        }
    }
    f
}

const PERSON: [u8; 3] = [240, 220, 20];

fn mask(x0: usize, x1: usize) -> Gray {
    let mut m = Gray::new(W, H);
    for y in 20..80 {
        for x in x0..x1 {
            m.data[y * W + x] = 255;
        }
    }
    m
}

fn run(look: &Look, frames: &[Rgb]) -> Vec<Rgb> {
    let mut e = Effect::named(&look.effect).expect("an effect");
    frames.iter().map(|f| e.run(f, look, Inputs { mask: None })).collect()
}

fn look(effect: &str) -> Look {
    Look { effect: effect.into(), ..Look::default() }
}

fn near(a: [u8; 3], b: [u8; 3], by: i32) -> bool {
    (0..3).all(|c| (a[c] as i32 - b[c] as i32).abs() <= by)
}

#[test]
fn the_empty_room_difference_finds_whoever_stands_in_front_of_it() {
    let room = room();
    let m = PlateMask::default().run(&with_person(&room, 60, 100, PERSON), &Arc::new(room)).unwrap();
    assert_eq!((m.data[50 * W + 80], m.data[50 * W + 10], m.data[5 * W + 80]), (255, 0, 0));
}

#[test]
fn the_cloak_shows_the_empty_room_where_you_are() {
    let room = room();
    let frame = with_person(&room, 60, 100, PERSON);
    let l = Look { plate: Some(Arc::new(room.clone())), edges: 0, ..look("cloak") };
    let out = &run(&l, std::slice::from_ref(&frame))[0];
    assert!(near(out.at(80, 50), room.at(80, 50), 10));
    assert_eq!(out.at(10, 50), frame.at(10, 50));
    // no empty room yet: left alone
    assert_eq!(run(&look("cloak"), std::slice::from_ref(&frame))[0], frame);
}

#[test]
fn the_cloak_uses_the_person_mask_when_the_vision_download_is_there() {
    let room = room();
    let frame = with_person(&room, 60, 100, PERSON);
    let l = Look { plate: Some(Arc::new(room.clone())), edges: 0, ..look("cloak") };
    let out = Effect::named("cloak").unwrap().run(&frame, &l, Inputs { mask: Some(mask(60, 100)) });
    assert!(near(out.at(80, 50), room.at(80, 50), 10));
}

#[test]
fn clones_stay_where_they_were_taken_and_you_stay_in_front() {
    let room = room();
    let first = with_person(&room, 10, 40, [255, 0, 0]);
    let clones = Clones::add(None, &first, &mask(10, 40));
    let second = with_person(&room, 60, 90, [0, 0, 255]);
    let clones = Clones::add(Some(&clones), &second, &mask(60, 90));
    assert_eq!(clones.count, 2);
    let now = with_person(&room, 70, 120, [0, 255, 0]);
    let l = Look { plate: Some(Arc::new(room.clone())), clones: Some(Arc::new(clones)), edges: 0, ..look("clones") };
    let out = &run(&l, &[now])[0];
    // the first clone, the second where you aren't, and you in front of it
    assert_eq!((out.at(20, 50), out.at(65, 50), out.at(80, 50)), ([255, 0, 0], [0, 0, 255], [0, 255, 0]));
    assert_eq!(out.at(5, 5), room.at(5, 5));
}

#[test]
fn light_painting_leaves_a_trail_that_fades_and_clear_starts_again() {
    let dark = Rgb::filled(W, H, [10, 10, 10]);
    let mut lit = dark.clone();
    for y in 40..44 {
        for x in 20..24 {
            lit.set(x, y, [255, 255, 255]);
        }
    }
    let l = Look { fade: 5, ..look("light") };
    let mut runner = crate::fun_pack::Runner::default();
    runner.run(&lit, &l, None);
    let mut after = runner.run(&dark, &l, None);
    // still there a frame later
    assert!(after.at(22, 42)[0] > 150);
    for _ in 0..60 {
        after = runner.run(&dark, &l, None);
    }
    // and gone after two seconds
    assert!(after.at(22, 42)[0] < 40);
    runner.run(&lit, &l, None);
    let cleared = runner.run(&dark, &Look { reset: 1, ..l.clone() }, None);
    assert_eq!(cleared.at(22, 42)[0], 10);
}

#[test]
fn long_light_trails_still_fade_all_the_way() {
    let dark = Rgb::filled(32, 18, [5, 5, 5]);
    let mut lit = dark.clone();
    lit.set(10, 9, [255, 255, 255]);
    let l = Look { fade: 10, ..look("light") };
    let mut e = Effect::named("light").unwrap();
    e.run(&lit, &l, Inputs { mask: None });
    let mut after = dark.clone();
    for _ in 0..3000 {
        after = e.run(&dark, &l, Inputs { mask: None });
    }
    // rounding used to leave a ghost of the trail behind for good
    assert_eq!(after.at(10, 9)[0], 5);
}

#[test]
fn slit_scan_shows_the_bottom_of_the_picture_from_further_back() {
    let frames: Vec<Rgb> = (0..25).map(|i| Rgb::filled(W, H, [i as u8 * 10; 3])).collect();
    let out = run(&Look { spread: 20, ..look("slit") }, &frames).pop().unwrap();
    // the top: now, the bottom: 20 frames back
    assert_eq!(out.at(80, 1)[0], 240);
    assert!((out.at(80, H - 1)[0] as i32 - 40).abs() <= 5);
    let across = run(&Look { spread: 20, across: true, ..look("slit") }, &frames).pop().unwrap();
    assert!(across.at(0, 45)[0] == 240 && (across.at(W - 1, 45)[0] as i32 - 40).abs() <= 5);
}

#[test]
fn the_stick_figure_is_drawn_on_paper_or_the_empty_room() {
    let l = Look { lines: vec![((0.2, 0.2), (0.8, 0.8))], heads: vec![((0.5, 0.3), 0.1)], ..look("stick") };
    let out = &run(&l, &[room()])[0];
    assert_eq!((out.at(W - 3, 2), out.at(80, 45)), (PAPER, INK));
    let r = room();
    let on_room = &run(&Look { plate: Some(Arc::new(r.clone())), ..look("stick") }, &[with_person(&r, 60, 100, PERSON)])[0];
    // nobody seen: just the room
    assert_eq!(*on_room, r);
}

fn big(frame: &Rgb) -> Rgb {
    frame.nearest(frame.w * 4, frame.h * 4)
}

#[test]
fn each_retro_look_keeps_the_size_and_changes_the_picture() {
    let frame = big(&with_person(&room(), 60, 100, PERSON));
    for effect in ["webcam", "vhs", "crt", "ascii"] {
        let out = run(&look(effect), &[frame.clone(), frame.clone()]).pop().unwrap();
        assert_eq!((out.w, out.h), (frame.w, frame.h), "{effect}");
        let mean = out.data.iter().zip(&frame.data).map(|(&a, &b)| (a as f64 - b as f64).abs()).sum::<f64>() / out.data.len() as f64;
        assert!(mean > 5.0, "{effect} changed the picture by {mean}");
    }
}

#[test]
fn the_1998_webcam_repeats_every_other_frame_without_handing_out_the_same_one() {
    let frame = big(&with_person(&room(), 60, 100, PERSON));
    let mut cam = Webcam::new();
    let (first, second, third) = (cam.run(&frame), cam.run(&frame), cam.run(&frame));
    assert!(first.data.iter().any(|&v| v != 0));
    assert_eq!(third.w, frame.w);
    // each new picture has its own noise
    assert_ne!(first, second);
    assert_eq!(cam.run(&frame).w, frame.w);
}

#[test]
fn a_jpeg_at_low_quality_blocks_the_picture_but_keeps_its_colours() {
    let mut img = Rgb::new(32, 32);
    for y in 0..32 {
        for x in 0..32 {
            img.set(x, y, [(x * 8) as u8, (y * 8) as u8, 128]);
        }
    }
    let out = jpeg(&img, 12);
    let mean = out.data.iter().zip(&img.data).map(|(&a, &b)| (a as f64 - b as f64).abs()).sum::<f64>() / out.data.len() as f64;
    assert!(mean > 1.0 && mean < 30.0, "{mean}");
    assert!(near(jpeg(&Rgb::filled(16, 16, [200, 60, 90]), 12).at(8, 8), [200, 60, 90], 10));
}

#[test]
fn the_ascii_picture_is_green_text_on_black() {
    let out = ascii(&big(&with_person(&room(), 60, 100, PERSON)), &mut None);
    assert!(out.data.chunks(3).all(|p| p[0] == 0 && p[2] == p[1] / 3));
    assert!(out.data.chunks(3).any(|p| p[1] > 100));
}

#[test]
fn a_plate_of_another_shape_is_never_stretched_over_the_picture() {
    let room = room();
    let turned = Rgb { w: H, h: W, data: with_person(&room, 60, 100, PERSON).data[..H * W * 3].to_vec() };
    let l = Look { plate: Some(Arc::new(room)), edges: 0, ..look("cloak") };
    assert_eq!(run(&l, std::slice::from_ref(&turned))[0], turned);
}

#[test]
fn the_crt_goes_black_outside_its_bulge_and_dims_the_corners() {
    let out = run(&look("crt"), &[Rgb::filled(320, 180, [200, 200, 200])]).pop().unwrap();
    assert!(out.at(160, 90)[1] > 100);
    assert!(out.at(0, 0).iter().all(|&v| v < 40));
}

#[test]
fn another_stream_starts_without_the_last_ones_history() {
    let mut e = Effect::named("slit").unwrap();
    let l = Look { spread: 20, ..look("slit") };
    for _ in 0..30 {
        e.run(&Rgb::filled(W, H, [200; 3]), &l, Inputs { mask: None });
    }
    let dark = Rgb::filled(W, H, [10; 3]);
    let fresh = Effect::named("slit").unwrap().run(&dark, &l, Inputs { mask: None });
    assert!(fresh.data.iter().all(|&v| v < 50));
}

#[test]
fn the_cloak_compares_with_the_room_it_has_now_not_one_that_was_there_before() {
    let (a, b) = (room(), Rgb::filled(W, H, [200, 30, 90]));
    let mut cloak = Effect::named("cloak").unwrap();
    let (first, second) = (Look { plate: Some(Arc::new(a.clone())), edges: 0, ..look("cloak") }, Look { plate: Some(Arc::new(b.clone())), edges: 0, ..look("cloak") });
    cloak.run(&a, &first, Inputs { mask: None });
    // a new room is taken; the frame is it, a little off but nobody in it
    let mut frame = b;
    frame.set(5, 5, [205, 35, 95]);
    assert!(cloak.run(&frame, &second, Inputs { mask: None }) == frame, "nobody there, so the frame is left as it is");
}

#[test]
fn the_webcams_height_rounds_halves_to_even_as_pythons_round_does() {
    // 481 * 320 / 640 = 240.5: Python's round() gives 240, and 483 gives 241.5 and so 242
    let mut cam = Webcam::new();
    cam.run(&Rgb::filled(640, 481, [90; 3]));
    let (small, _) = cam.held.as_ref().unwrap();
    assert_eq!((small.w, small.h), (320, 240));
    let mut cam = Webcam::new();
    cam.run(&Rgb::filled(640, 483, [90; 3]));
    assert_eq!(cam.held.as_ref().unwrap().0.h, 242);
}

#[test]
fn reused_buffers_leave_nothing_of_the_last_frame_behind() {
    let busy = |seed: usize| Rgb { w: 320, h: 180, data: (0..320 * 180 * 3).map(|i| (i * (i + seed) % 251) as u8).collect() };
    for effect in ["crt", "ascii"] {
        let frames = [busy(1), busy(2), busy(3)];
        let plain = run(&look(effect), &frames);
        let mut held = Vec::new();
        let reused = telescope_core::image::with_spares(&mut held, || run(&look(effect), &frames));
        assert_eq!(plain, reused, "{effect}");
    }
    let frames = [busy(1), busy(2)];
    let mut held = Vec::new();
    telescope_core::image::with_spares(&mut held, || run(&look("crt"), &frames));
    assert!(!held.is_empty(), "the buffers came back: {}", held.len());
}

// ── Slit: copying each band's span, against the scan it replaced ──

fn slit_reference(frames: &VecDeque<Rgb>, across: bool, spread: usize, sw: usize, sh: usize) -> Rgb {
    let size = if across { sw } else { sh };
    let bands = 45.min(size);
    let mut out = Rgb::new(sw, sh);
    let last = frames.len() - 1;
    for b in 0..bands {
        let (lo, hi) = (b * size / bands, (b + 1) * size / bands);
        let src = &frames[last.min(b * spread / (bands - 1).max(1))];
        for y in 0..sh {
            for x in 0..sw {
                let inside = if across { (lo..hi).contains(&x) } else { (lo..hi).contains(&y) };
                if inside {
                    out.set(x, y, src.at(x, y));
                }
            }
        }
    }
    out
}

fn slit_history(sw: usize, sh: usize, count: usize) -> VecDeque<Rgb> {
    (0..count).map(|n| Rgb { w: sw, h: sh, data: (0..sw * sh * 3).map(|i| (i.wrapping_mul(31).wrapping_add(n * 97) >> 3) as u8).collect() }).collect()
}

#[test]
fn slit_bands_match_the_full_scan_byte_for_byte() {
    for (sw, sh) in [(1, 1), (7, 5), (44, 46), (45, 45), (100, 61), (160, 90)] {
        for spread in [1usize, 2, 7, 30] {
            for count in [1, spread + 1] {
                let frames = slit_history(sw, sh, count);
                for across in [true, false] {
                    assert!(slit_bands(&frames, across, spread, sw, sh) == slit_reference(&frames, across, spread, sw, sh), "{sw}x{sh} spread {spread} frames {count} across {across}");
                }
            }
        }
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_slit() {
    for (sw, sh) in [(960usize, 540usize), (1920, 1080)] {
        let frames = slit_history(sw, sh, 31);
        for across in [true, false] {
            let best = |f: &dyn Fn() -> Rgb| (0..5).map(|_| {
                let start = std::time::Instant::now();
                std::hint::black_box(f());
                start.elapsed().as_secs_f64() * 1000.0
            }).fold(f64::MAX, f64::min);
            let old = best(&|| slit_reference(&frames, across, 30, sw, sh));
            let new = best(&|| slit_bands(&frames, across, 30, sw, sh));
            println!("slit bands {}x{} across {across}: old {old:.2} ms, new {new:.2} ms", sw * 2, sh * 2);
        }
    }
}

// ── The fused and reduced versions against what they replaced ──

/// A softer, photo-like picture: smooth colour, a few hard edges and a little grain.
fn photo(w: usize, h: usize) -> Rgb {
    let mut r = Rgb::new(w, h);
    let mut n = 99u32;
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let sky = [90.0 + 120.0 * fy, 140.0 + 80.0 * fy, 230.0 - 60.0 * fy];
            let inside = ((fx - 0.5).powi(2) + (fy - 0.6).powi(2)).sqrt() < 0.22;
            let base = if inside { [215.0, 170.0, 60.0 + 80.0 * fx] } else { sky };
            let grain = (n >> 29) as f32 - 3.0;
            r.set(x, y, base.map(|c| (c + grain).clamp(0.0, 255.0) as u8));
        }
    }
    r
}

/// The largest and the mean difference between two pictures' bytes.
fn gap(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let max = a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y)).max().unwrap_or(0);
    (max, a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y) as f64).sum::<f64>() / a.len().max(1) as f64)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

/// Light painting as it was: separate passes, a copy for each.
fn light_reference(trail: &mut Option<Rgb>, frame: &Rgb, look: &Look) -> Rgb {
    let (w, h) = (frame.w, frame.h);
    let level = (150 + look.brightness * 10) as u8;
    let lit = frame.gray().above(level);
    let mut lights = Rgb::new(w, h);
    for (i, m) in lit.data.iter().enumerate() {
        if *m != 0 {
            lights.data[i * 3..i * 3 + 3].copy_from_slice(&frame.data[i * 3..i * 3 + 3]);
        }
    }
    let next = match trail.as_ref().filter(|t| (t.w, t.h) == (w, h)) {
        None => lights,
        Some(t) => {
            let keep = (0.80 + look.fade as f64 * 0.019) as f32;
            let data = t.data.iter().zip(&lights.data).map(|(&old, &new)| ((old as f32 * keep).round_ties_even().min(255.0) as u8).saturating_sub(1).max(new)).collect();
            Rgb { w, h, data }
        }
    };
    let out = Rgb { w, h, data: frame.data.iter().zip(&next.data).map(|(&a, &b)| a.max(b)).collect() };
    *trail = Some(next);
    out
}

#[test]
fn light_painting_in_one_pass_matches_the_passes_it_replaced_byte_for_byte() {
    // 640 x 360 is tall enough to be spread over the workers; 161 x 91 is not
    for (w, h) in [(640usize, 360usize), (161, 91)] {
        for fade in [1, 5, 10] {
            let l = Look { fade, brightness: 3, ..look("light") };
            let (mut now, mut then) = (Effect::named("light").unwrap(), None);
            for i in 0..6 {
                let mut frame = photo(w, h);
                // a light that moves
                for y in 0..h / 4 {
                    for x in 0..w / 8 {
                        frame.set((x + i * w / 10).min(w - 1), y + h / 3, [255, 250, 240]);
                    }
                }
                let want = light_reference(&mut then, &frame, &l);
                assert!(now.run(&frame, &l, Inputs { mask: None }) == want, "{w}x{h} fade {fade} frame {i}");
            }
        }
    }
}

#[test]
fn light_painting_starts_afresh_on_a_new_size() {
    let l = look("light");
    let mut e = Effect::named("light").unwrap();
    e.run(&Rgb::filled(64, 36, [255; 3]), &l, Inputs { mask: None });
    let dark = Rgb::filled(32, 18, [10; 3]);
    assert!(e.run(&dark, &l, Inputs { mask: None }) == dark);
}

/// The CRT as it was: the whole bent picture held, and a 9 x 9 glow of it at full size.
fn crt_reference(c: &Crt, frame: &Rgb) -> Rgb {
    let (w, h) = (c.w, c.h);
    let mut bent = Rgb::scratch(w, h);
    c.bend.run(&frame.data[..w * h * 3], &mut bent.data);
    let glow = bent.box_blur(9, 9);
    let mut out = Rgb::scratch(w, h);
    let rl = w * 3;
    over_rows(&mut out.data, rl, |y, line| crt_row(&bent.data[y * rl..][..rl], &glow.data[y * rl..][..rl], &c.shade[y * rl..][..rl], line));
    out
}

#[test]
fn the_crt_with_its_glow_at_a_third_of_the_size_looks_the_same() {
    for (w, h) in [(960usize, 540usize), (641, 361)] {
        let frame = photo(w, h);
        let crt = Crt::new(w, h);
        let (got, want) = (crt.run(&frame), crt_reference(&crt, &frame));
        let (max, mean) = gap(&got.data, &want.data);
        println!("crt {w}x{h}: max {max}, mean {mean:.4}, psnr {:.1} dB", psnr(&got.data, &want.data));
        assert!(mean < 0.2 && psnr(&got.data, &want.data) > 48.0, "{w}x{h}: max {max} mean {mean}");
    }
    // the hard edges of a picture glow into their surroundings the same way
    let mut edges = Rgb::new(480, 270);
    for y in 0..270 {
        for x in 0..480 {
            edges.set(x, y, if (x / 40 + y / 30) % 2 == 0 { [250, 250, 250] } else { [5, 5, 5] });
        }
    }
    let crt = Crt::new(480, 270);
    let (got, want) = (crt.run(&edges), crt_reference(&crt, &edges));
    let (max, mean) = gap(&got.data, &want.data);
    println!("crt checkerboard: max {max}, mean {mean:.4}, psnr {:.1} dB", psnr(&got.data, &want.data));
    assert!(mean < 0.8 && psnr(&got.data, &want.data) > 38.0, "max {max} mean {mean}");
}

#[test]
fn the_crt_never_lights_what_is_outside_its_bulge() {
    let crt = Crt::new(320, 180);
    let out = crt.run(&Rgb::filled(320, 180, [255; 3]));
    for y in 0..180 {
        let (from, to) = crt.bend.span(y);
        assert!(out.data[y * 960..][..from * 3].iter().all(|&v| v == 0) && out.data[y * 960 + to * 3..][..960 - to * 3].iter().all(|&v| v == 0), "row {y}");
    }
}

/// ASCII as it was: a pixel at a time.
fn ascii_reference(levels: &[u8], w: usize, h: usize, held: &mut Option<AsciiGlyphs>) -> Rgb {
    let (cw, ch, cols, rows) = ascii_cells(w, h);
    let mut out = Rgb::new(w, h);
    *held = Some(AsciiGlyphs::new(cw, ch));
    let glyphs = &held.as_ref().unwrap().glyphs;
    for row in 0..rows {
        for col in 0..cols {
            let index = levels[row * cols + col] as usize * ASCII_RAMP.len() / 256;
            let glyph = &glyphs[index * cw * ch..(index + 1) * cw * ch];
            for y in 0..ch {
                for x in 0..cw {
                    let text = glyph[y * cw + x];
                    out.set(col * cw + x, row * ch + y, [0, text, text / 3]);
                }
            }
        }
    }
    out
}

#[test]
fn ascii_rows_copied_whole_draw_the_same_picture_as_one_pixel_at_a_time() {
    // widths that leave a margin to the right of the last cell, and heights a margin below
    for (w, h) in [(1280usize, 720usize), (1003, 517), (3840, 2160)] {
        let (_, _, cols, rows) = ascii_cells(w, h);
        let levels: Vec<u8> = (0..cols * rows).map(|i| (i * 37 % 256) as u8).collect();
        let want = ascii_reference(&levels, w, h, &mut None);
        assert!(ascii_drawn(&levels, w, h, &mut None) == want, "{w}x{h}");
    }
}

#[test]
fn ascii_from_the_luma_plane_picks_the_characters_the_rgb_picture_does() {
    let (w, h) = (960usize, 540usize);
    let picture = photo(w, h);
    let mut frame = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(w as u32, h as u32)]);
    picture.write_to(&mut frame);
    // what the RGB path sees of the same frame
    let seen = Rgb::from_frame(&frame).unwrap();
    let from_rgb = ascii(&seen, &mut None);
    let from_luma = ascii_from_luma(&frame.data[..w * h], w, h, &mut None);
    let differing = from_rgb.data.chunks(3).zip(from_luma.data.chunks(3)).filter(|(a, b)| a != b).count() as f64 / (w * h) as f64;
    println!("ascii: {:.2}% of pixels differ", differing * 100.0);
    assert!(differing < 0.03, "{differing}");
    // and through the effect's frame path
    let mut e = Effect::named("ascii").unwrap();
    assert!(e.run_frame(&mut frame, &look("ascii")).is_some());
    let drawn = Rgb::from_frame(&frame).unwrap();
    assert!(drawn.data.chunks(3).any(|p| p[1] > 100));
}

#[test]
fn vhs_on_the_planes_looks_like_vhs_on_rgb() {
    let (w, h) = (960usize, 540usize);
    let picture = photo(w, h);
    let mut frame = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(w as u32, h as u32)]);
    picture.write_to(&mut frame);
    let seen = Rgb::from_frame(&frame).unwrap();
    for t in 0..4 {
        let (mut by_rgb, mut by_planes) = (Vhs { t, noise: Noise(7) }, Vhs { t, noise: Noise(7) });
        let want = by_rgb.run(&seen);
        let mut planes = frame.clone();
        assert!(by_planes.run_i420(planes.data_mut(), w, h));
        let got = Rgb::from_frame(&planes).unwrap();
        let (max, mean) = gap(&got.data, &want.data);
        let p = psnr(&got.data, &want.data);
        println!("vhs frame {t}: max {max}, mean {mean:.3}, psnr {p:.1} dB");
        assert!(mean < 1.0 && p > 44.0, "frame {t}: max {max} mean {mean} psnr {p}");
    }
}

#[test]
fn vhs_leaves_other_formats_and_short_buffers_to_the_rgb_path() {
    let mut v = Vhs::new();
    assert!(!v.run_i420(&mut [0u8; 10], 16, 16));
    let mut frame = Frame::new(4, 4, PixelFormat::Rgba, vec![0; 64]);
    assert!(Effect::named("vhs").unwrap().run_frame(&mut frame, &look("vhs")).is_none());
}

#[test]
fn the_stick_figure_is_drawn_again_only_when_what_it_is_drawn_from_changes() {
    let r = room();
    let l = Look { lines: vec![((0.2, 0.2), (0.8, 0.8))], heads: vec![((0.5, 0.3), 0.1)], ..look("stick") };
    let mut e = Effect::named("stick").unwrap();
    let (first, second) = (e.run(&r, &l, Inputs { mask: None }), e.run(&r, &l, Inputs { mask: None }));
    assert!(first == second);
    // each hands out a picture of its own
    assert_ne!(first.data.as_ptr(), second.data.as_ptr());
    let moved = Look { lines: vec![((0.2, 0.2), (0.3, 0.9))], ..l.clone() };
    assert_ne!(e.run(&r, &moved, Inputs { mask: None }), first);
    // written over a frame it is the same picture in the frame's format
    let mut frame = Frame::new(W as u32, H as u32, PixelFormat::I420, vec![7; PixelFormat::I420.size(W as u32, H as u32)]);
    assert!(e.run_frame(&mut frame, &moved).is_some());
    let mut want = Frame::new(W as u32, H as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(W as u32, H as u32)]);
    e.run(&r, &moved, Inputs { mask: None }).write_to(&mut want);
    assert_eq!(frame.data, want.data);
    // another size is not covered by it
    let mut small = Frame::new(8, 8, PixelFormat::I420, vec![0; PixelFormat::I420.size(8, 8)]);
    assert!(Effect::Stick(None).run_frame(&mut small, &moved).is_some());
    assert_eq!((small.width, small.height), (8, 8));
}

#[test]
fn nearest_with_rows_shared_matches_the_pixel_by_pixel_version() {
    for (w, h, w2, h2) in [(320usize, 180usize, 1920usize, 1080usize), (7, 5, 30, 23), (9, 4, 4, 2), (1, 1, 5, 3)] {
        let src = bench_picture(w, h);
        let got = src.nearest(w2, h2);
        for y in 0..h2 {
            for x in 0..w2 {
                assert_eq!(got.at(x, y), src.at((x * w / w2).min(w - 1), (y * h / h2).min(h - 1)), "{w}x{h} to {w2}x{h2} at {x},{y}");
            }
        }
    }
}

/// A picture with texture at any size: a gradient, some blocks and a little noise.
fn bench_picture(w: usize, h: usize) -> Rgb {
    let mut r = Rgb::new(w, h);
    let mut n = 12345u32;
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let block = ((x / (w / 12).max(1) + y / (h / 7).max(1)) % 3) as u32 * 40;
            let v = [(x * 255 / w) as u32 + block, (y * 255 / h) as u32 + (n >> 28), 90 + block + (n >> 29)];
            r.set(x, y, v.map(|c| c.min(255) as u8));
        }
    }
    r
}

fn timed(f: &mut dyn FnMut()) -> f64 {
    f();
    let mut runs: Vec<f64> = (0..7).map(|_| {
        let start = std::time::Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1000.0
    }).collect();
    runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    runs[runs.len() / 2]
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_fun_effects() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = bench_picture(w, h);
        let plate = Arc::new(bench_picture(w, h));
        let mut moved = frame.clone();
        for y in h / 4..h * 3 / 4 {
            for x in w / 3..w / 2 {
                moved.set(x, y, [240, 220, 20]);
            }
        }
        for name in ["light", "webcam", "vhs", "crt", "ascii", "stick", "clones", "cloak"] {
            let look = Look { effect: name.into(), plate: Some(plate.clone()), lines: vec![((0.3, 0.2), (0.5, 0.7))], heads: vec![((0.4, 0.15), 0.05)], ..Look::default() };
            let mut e = Effect::named(name).unwrap();
            let mut i = 0;
            let ms = timed(&mut || {
                i += 1;
                let input = if i % 2 == 0 { &moved } else { &frame };
                std::hint::black_box(e.run(input, &look, Inputs { mask: None }));
            });
            println!("fun {name:<7} {w}x{h}: {ms:.2} ms");
        }
        // the whole step on an I420 frame: the way through RGB and back, and the way that works on the frame's own planes
        let i420 = {
            let mut f = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(w as u32, h as u32)]);
            frame.write_to(&mut f);
            f
        };
        for name in ["vhs", "ascii", "stick", "crt", "light", "webcam"] {
            let look = Look { effect: name.into(), lines: vec![((0.3, 0.2), (0.5, 0.7))], heads: vec![((0.4, 0.15), 0.05)], ..Look::default() };
            let mut e = Effect::named(name).unwrap();
            let mut f = i420.clone();
            let through_rgb = timed(&mut || {
                let rgb = Rgb::from_frame(&f).unwrap();
                e.run(&rgb, &look, Inputs { mask: None }).write_to(&mut f);
            });
            let mut e = Effect::named(name).unwrap();
            let mut f = i420.clone();
            let planes = timed(&mut || {
                if e.run_frame(&mut f, &look).is_none() {
                    let rgb = Rgb::from_frame(&f).unwrap();
                    e.run(&rgb, &look, Inputs { mask: None }).write_to(&mut f);
                }
            });
            println!("step {name:<7} {w}x{h}: through rgb {through_rgb:.2} ms, frame path {planes:.2} ms");
        }
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_crt_parts() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = bench_picture(w, h);
        let crt = Crt::new(w, h);
        let src = &frame.data[..w * h * 3];
        let mut bent = Rgb::scratch(w, h);
        println!("crt parts {w}x{h}: bend only {:.2} ms", timed(&mut || crt.bend.run(src, &mut bent.data)));
        println!("crt parts {w}x{h}: glow only {:.2} ms", timed(&mut || drop(std::hint::black_box(crt.glow.of(src, w, h)))));
        let glow = bent.box_blur(9, 9);
        let mut out = Rgb::scratch(w, h);
        let rl = w * 3;
        println!("crt parts {w}x{h}: shade pass only {:.2} ms", timed(&mut || over_rows(&mut out.data, rl, |y, line| crt_row(&bent.data[y * rl..][..rl], &glow.data[y * rl..][..rl], &crt.shade[y * rl..][..rl], line))));
        println!("crt parts {w}x{h}: full {:.2} ms", timed(&mut || drop(std::hint::black_box(crt.run(&frame)))));
        println!("crt parts {w}x{h}: reference {:.2} ms", timed(&mut || drop(std::hint::black_box(crt_reference(&crt, &frame)))));
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_vhs_parts() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = bench_picture(w, h);
        let (sw, sh) = (w / 3, h / 2);
        let mut f = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(w as u32, h as u32)]);
        frame.write_to(&mut f);
        let (cw, ch) = (w / 2, h / 2);
        let mut v = Vhs::new();
        println!("vhs parts {w}x{h}: y area {:.2} ms", timed(&mut || drop(std::hint::black_box(resize_area(&f.data[..w * h], w, h, 1, sw, sh)))));
        println!("vhs parts {w}x{h}: chroma area {:.2} ms", timed(&mut || drop(std::hint::black_box(resize_area(&f.data[w * h..][..cw * ch], cw, ch, 1, sw, sh)))));
        let small = Gray { w: sw, h: sh, data: resize_area(&f.data[..w * h], w, h, 1, sw, sh) };
        println!("vhs parts {w}x{h}: box blur {:.2} ms", timed(&mut || drop(std::hint::black_box(small.box_blur(3, 1)))));
        let mut up = vec![0u8; w * h];
        println!("vhs parts {w}x{h}: y up {:.2} ms", timed(&mut || resize_linear_into(&small.data, sw, sh, 1, &mut up, w, (0, 0, w, h))));
        println!("vhs parts {w}x{h}: all {:.2} ms", timed(&mut || {
            v.run_i420(f.data_mut(), w, h);
        }));
        println!("vhs parts {w}x{h}: rgb path {:.2} ms", timed(&mut || drop(std::hint::black_box(v.run(&frame)))));
    }
}
