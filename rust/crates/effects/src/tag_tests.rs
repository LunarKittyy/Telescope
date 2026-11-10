use super::*;

const CANVASES: [(u32, u32); 7] = [(640, 360), (1280, 720), (1920, 1080), (3840, 2160), (1080, 1920), (720, 720), (320, 180)];

fn spec() -> Spec {
    Spec { name: "Luna Kozankiewicz".into(), pronouns: "she/her".into(), second: "Telescope developer".into(), ..Default::default() }
}

/// None when this machine has no font to measure with, which skips the test.
fn fonts() -> Option<Fonts> {
    let fonts = Fonts::system();
    if fonts.is_none() {
        eprintln!("no font on this machine: the name tag drawing is skipped");
    }
    fonts
}

fn rgba(w: u32, h: u32, grey: u8) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba, [grey, grey, grey, 255].repeat((w * h) as usize))
}

#[test]
fn the_tag_stays_on_the_canvas_where_it_was_put() {
    let Some(fonts) = fonts() else { return };
    for (w, h) in CANVASES {
        for position in POSITIONS {
            let lay = layout_with(&Spec { position: position.into(), ..spec() }, w, h, &fonts).unwrap();
            let b = lay.box_;
            let (cx, cy) = b.centre();
            let (wf, hf) = (w as f64, h as f64);
            assert!(0.0 <= b.left() && b.right() <= wf && 0.0 <= b.top() && b.bottom() <= hf, "{position} {w}x{h}");
            assert_eq!(cy > hf / 2.0, position.starts_with("bottom"), "{position} {w}x{h}");
            if position.ends_with("left") {
                assert!(cx < wf / 2.0);
            } else if position.ends_with("right") {
                assert!(cx > wf / 2.0);
            } else {
                assert!((cx - wf / 2.0).abs() <= 1.0);
            }
        }
    }
}

#[test]
fn the_text_sits_inside_the_box() {
    let Some(fonts) = fonts() else { return };
    for (w, h) in CANVASES {
        let lay = layout_with(&spec(), w, h, &fonts).unwrap();
        for piece in [&lay.name, &lay.pronouns, &lay.second] {
            let at = piece.as_ref().unwrap();
            assert!(lay.box_.left() <= at.x && at.baseline <= lay.box_.bottom());
        }
        // the second line goes under the name
        assert!(lay.second.unwrap().baseline > lay.name.unwrap().baseline);
    }
}

#[test]
fn it_grows_with_the_canvas_and_the_size_picked() {
    let Some(fonts) = fonts() else { return };
    let small = layout_with(&spec(), 1280, 720, &fonts).unwrap().box_;
    let big = layout_with(&spec(), 2560, 1440, &fonts).unwrap().box_;
    assert!((big.h / (small.h * 2.0) - 1.0).abs() < 0.08, "{} {}", big.h, small.h);
    let sizes: Vec<f64> = ["small", "medium", "large"].iter().map(|s| layout_with(&Spec { size: s.to_string(), ..spec() }, 1280, 720, &fonts).unwrap().box_.h).collect();
    assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2]);
}

#[test]
fn a_long_name_is_cut_to_fit_the_width() {
    let Some(fonts) = fonts() else { return };
    let name = "Wolfeschlegelsteinhausenbergerdorff ".repeat(4);
    let lay = layout_with(&Spec { name, ..spec() }, 640, 360, &fonts).unwrap();
    assert!(lay.box_.right() <= 640.0 && lay.name.unwrap().text.ends_with('\u{2026}'));
}

#[test]
fn nothing_to_show_without_any_text() {
    let Some(fonts) = fonts() else { return };
    assert!(layout_with(&Spec::default(), 1280, 720, &fonts).is_none());
    assert!(render_with(&Spec { name: "   ".into(), ..Default::default() }, 1280, 720, &fonts).is_none());
    assert!(layout_with(&Spec { second: "Just a role".into(), ..Default::default() }, 1280, 720, &fonts).unwrap().name.is_none());
}

#[test]
fn a_right_to_left_name_starts_on_the_right() {
    let Some(fonts) = fonts() else { return };
    let arabic = |s: &str| s.to_string();
    let lay = layout_with(&Spec { name: arabic("\u{633}\u{627}\u{631}\u{629} \u{623}\u{62d}\u{645}\u{62f}"), pronouns: arabic("\u{647}\u{64a}"), style: "bar".into(), ..Default::default() }, 1280, 720, &fonts).unwrap();
    assert!(right_to_left("\u{633}\u{627}\u{631}\u{629}") && !right_to_left("Luna") && !right_to_left("\u{5c71}\u{7530}"));
    // read from the right: the name first
    assert!(lay.pronouns.unwrap().x < lay.name.unwrap().x);
    assert_eq!(lay.stripe.unwrap().right(), lay.box_.right());
}

#[test]
fn a_whitespace_name_doesnt_decide_the_direction() {
    let Some(fonts) = fonts() else { return };
    let lay = layout_with(&Spec { name: "   ".into(), pronouns: "\u{647}\u{64a}".into(), ..Default::default() }, 1280, 720, &fonts).unwrap();
    // laid out right to left, by the pronouns
    assert_eq!(lay.stripe.unwrap().right(), lay.box_.right());
}

#[test]
fn names_in_other_scripts_get_drawn() {
    let Some(fonts) = fonts() else { return };
    for name in ["山田 花子", "नीलम शर्मा", "Ζωή Παπαδοπούλου", "Олена Коваль", "김민지"] {
        let s = Spec { name: name.into(), style: "bar".into(), ..Default::default() };
        let tag = render_with(&s, 1280, 720, &fonts).unwrap();
        let lay = layout_with(&s, 1280, 720, &fonts).unwrap();
        // nothing cut
        assert_eq!(lay.name.as_ref().unwrap().text, name);
        let (left, top) = ((lay.box_.left() as usize + 1).saturating_sub(tag.x), (lay.box_.top() as usize + 1).saturating_sub(tag.y));
        let (right, bottom) = (lay.box_.right() as usize - 1 - tag.x, lay.box_.bottom() as usize - 1 - tag.y);
        // the box covers its area
        for y in top..bottom {
            for x in left..right {
                assert!(tag.keep[y * tag.width + x] < 255, "{name} {x},{y}");
            }
        }
        let text_left = left + (lay.em * 0.9) as usize;
        let mut bright = 0;
        for y in top..bottom {
            for x in text_left..right {
                let t = y * tag.width + x;
                bright += (tag.ink[t * 3..t * 3 + 3].iter().max().copied().unwrap() > 200) as usize;
            }
        }
        // the glyphs are there
        assert!(bright as f64 > lay.em * 3.0, "{name}: {bright}");
    }
}

#[test]
fn drawing_only_touches_the_tile() {
    let Some(fonts) = fonts() else { return };
    let mut frame = rgba(1280, 720, 100);
    let tag = render_with(&spec(), 1280, 720, &fonts).unwrap();
    apply(&mut frame, &tag);
    let mut touched = 0;
    for y in 0..720usize {
        for x in 0..1280usize {
            let inside = x >= tag.x && x < tag.x + tag.width && y >= tag.y && y < tag.y + tag.height;
            let px = &frame.data[(y * 1280 + x) * 4..(y * 1280 + x) * 4 + 3];
            if !inside {
                assert_eq!(px, [100, 100, 100], "{x},{y}");
            } else if px != [100, 100, 100] {
                touched += 1;
            }
        }
    }
    assert!(touched > 0);
}

#[test]
fn a_frame_somebody_else_holds_is_copied() {
    let Some(fonts) = fonts() else { return };
    let original = rgba(640, 360, 50);
    let mut frame = original.clone();
    apply(&mut frame, &render_with(&spec(), 640, 360, &fonts).unwrap());
    assert!(original.data.chunks(4).all(|p| p == [50, 50, 50, 255]));
    assert_ne!(*original.data, *frame.data);
}

#[test]
fn the_tag_lands_on_i420_frames_the_way_it_does_on_rgb_ones() {
    let Some(fonts) = fonts() else { return };
    let tag = render_with(&Spec { style: "light".into(), ..spec() }, 640, 360, &fonts).unwrap();
    let mut grey = rgba(640, 360, 120);
    apply(&mut grey, &tag);
    // an I420 frame of the same grey: Y 16 + 120 * 219 / 255, neutral chroma
    let y = (16.0 + 120.0 * 219.0 / 255.0f32).round() as u8;
    let mut data = vec![y; 640 * 360];
    data.extend(vec![128u8; 2 * 320 * 180]);
    let mut yuv = Frame::new(640, 360, PixelFormat::I420, data);
    apply(&mut yuv, &tag);
    // the light tag is a pale card with dark text: a pixel inside it should be about as bright in both
    let (px, py) = (tag.x + 3, tag.y + tag.height / 2);
    let rgb_grey = grey.data[(py * 640 + px) * 4] as f32;
    let luma = (yuv.data[py * 640 + px] as f32 - 16.0) * 255.0 / 219.0;
    assert!((rgb_grey - luma).abs() < 4.0, "{rgb_grey} {luma}");
    // and far from the tag nothing changed
    assert_eq!(yuv.data[10 * 640 + 10], y);
}

#[test]
fn the_cache_paints_once_until_something_changes() {
    if fonts().is_none() {
        return;
    }
    let mut cache = TagCache::default();
    let first = cache.get(&spec(), 1280, 720).unwrap();
    assert!(std::sync::Arc::ptr_eq(&cache.get(&spec(), 1280, 720).unwrap(), &first) && cache.renders == 1);
    let changed = [
        Spec { name: "Alex".into(), ..spec() }, Spec { pronouns: "they/them".into(), ..spec() }, Spec { second: String::new(), ..spec() },
        Spec { style: "light".into(), ..spec() }, Spec { position: "top_right".into(), ..spec() }, Spec { size: "large".into(), ..spec() },
    ];
    for c in &changed {
        assert!(!std::sync::Arc::ptr_eq(&cache.get(c, 1280, 720).unwrap(), &first));
    }
    let renders = cache.renders;
    // the canvas changed size
    assert!(!std::sync::Arc::ptr_eq(&cache.get(&spec(), 1920, 1080).unwrap(), &first));
    assert_eq!(cache.renders, renders + 1);
}

#[test]
fn the_shadow_style_adds_a_soft_halo_under_the_text() {
    let Some(fonts) = fonts() else { return };
    let tag = render_with(&Spec { style: "shadow".into(), ..spec() }, 1280, 720, &fonts).unwrap();
    // some pixels are mostly the halo: dark, a little covered
    let halo = (0..tag.width * tag.height).filter(|t| tag.ink[t * 3] < 20 && tag.keep[*t] < 250 && tag.keep[*t] > 100).count();
    assert!(halo > 100, "{halo}");
}

#[test]
fn it_uses_the_apps_fonts() {
    for weight in [telescope_core::text::Weight::Regular, telescope_core::text::Weight::SemiBold] {
        let files = telescope_core::text::app_font_files(weight);
        assert!(files[0].starts_with("Inter-"), "{files:?}");
    }
    // Inter first, when the system has it
    let (Some(inter), Some(app)) = (telescope_core::text::font_named(&["Inter-Regular.ttf"]), telescope_core::text::app_font(telescope_core::text::Weight::Regular)) else {
        eprintln!("Inter isn't installed here: the font itself isn't compared");
        return;
    };
    assert_eq!(Face::em(inter, 40.0).advance("Luna"), Face::em(app, 40.0).advance("Luna"));
}

/// A person's name can have an emoji in it: the tile holds its colours, not the text's.
#[test]
fn an_emoji_in_a_name_keeps_its_colours() {
    let spec = Spec { name: "\u{1f319}\u{1f319}\u{1f319}".into(), style: "shadow".into(), ..Spec::default() };
    let Some(tag) = render(&spec, 1280, 720) else { return };
    let yellow = tag.ink.chunks(3).filter(|p| p[0] > 180 && p[2] < 120).count();
    if telescope_core::text::app_font(telescope_core::text::Weight::Regular).is_some() && yellow == 0 {
        // A machine without a colour font draws nothing here
        assert!(!std::fs::read_dir("/usr/share/fonts/google-noto-color-emoji-fonts").is_ok_and(|mut d| d.next().is_some()));
    }
}

/// The same picture in the Windows camera's format gets the same tag.
#[test]
fn a_tag_goes_on_nv12_as_on_i420() {
    let (w, h) = (64usize, 48usize);
    let tag = Tag::new(8, 8, 16, 16, vec![200; 16 * 16 * 3], vec![40; 16 * 16]);
    let mut i420 = vec![120u8; w * h];
    i420.extend(vec![90u8; (w / 2) * (h / 2)]);
    i420.extend(vec![170u8; (w / 2) * (h / 2)]);
    let mut nv12 = vec![120u8; w * h];
    nv12.extend([90u8, 170].repeat((w / 2) * (h / 2)));
    let (mut a, mut b) = (Frame::new(w as u32, h as u32, PixelFormat::I420, i420), Frame::new(w as u32, h as u32, PixelFormat::Nv12, nv12));
    apply(&mut a, &tag);
    apply(&mut b, &tag);
    assert_eq!(a.data[..w * h], b.data[..w * h]);
    let (cw, ch) = (w / 2, h / 2);
    for i in 0..cw * ch {
        assert_eq!((a.data[w * h + i], a.data[w * h + cw * ch + i]), (b.data[w * h + i * 2], b.data[w * h + i * 2 + 1]));
    }
    assert_ne!(a.data[8 * w + 8], 120);
}

// ── Cached YUV terms, against the per-frame version they replaced ──

/// The apply this replaced, kept to compare every byte against.
fn apply_reference(frame: &mut Frame, tag: &Tag) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if tag.x + tag.width > w || tag.y + tag.height > h {
        return;
    }
    match frame.format {
        PixelFormat::Rgba | PixelFormat::Bgra => {
            let swap = frame.format == PixelFormat::Bgra;
            let data = frame.data_mut();
            for ty in 0..tag.height {
                for tx in 0..tag.width {
                    let t = ty * tag.width + tx;
                    let i = ((tag.y + ty) * w + tag.x + tx) * 4;
                    for c in 0..3 {
                        let ink = tag.ink[t * 3 + if swap { 2 - c } else { c }];
                        data[i + c] = blend(data[i + c], tag.keep[t], ink);
                    }
                }
            }
        }
        PixelFormat::I420 | PixelFormat::Nv12 => {
            let nv12 = frame.format == PixelFormat::Nv12;
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let data = frame.data_mut();
            let (luma, chroma) = data.split_at_mut(w * h);
            // Where a chroma cell's U and V are
            let places = |i: usize| if nv12 { (i * 2, i * 2 + 1) } else { (i, cw * ch + i) };
            for ty in 0..tag.height {
                for tx in 0..tag.width {
                    let t = ty * tag.width + tx;
                    let alpha = 255.0 - tag.keep[t] as f32;
                    let (y, _, _) = yuv_of([tag.ink[t * 3], tag.ink[t * 3 + 1], tag.ink[t * 3 + 2]]);
                    let i = (tag.y + ty) * w + tag.x + tx;
                    let under = luma[i] as f32;
                    luma[i] = (under * tag.keep[t] as f32 / 255.0 + y + 16.0 * alpha / 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            // Chroma is shared by 2 x 2 pixels: each takes the average of the tag over the pixels it covers
            let (cx0, cy0) = (tag.x / 2, tag.y / 2);
            let (cx1, cy1) = ((tag.x + tag.width).div_ceil(2), (tag.y + tag.height).div_ceil(2));
            for cy in cy0..cy1.min(ch) {
                for cx in cx0..cx1.min(cw) {
                    let (mut su, mut sv, mut keep, mut alpha, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                        // Pixels of the 2 x 2 that the tile doesn't cover count as no tag
                        let inside = x >= tag.x && x < tag.x + tag.width && y >= tag.y && y < tag.y + tag.height;
                        let (k, a, u, v) = if inside {
                            let t = (y - tag.y) * tag.width + x - tag.x;
                            let (_, u, v) = yuv_of([tag.ink[t * 3], tag.ink[t * 3 + 1], tag.ink[t * 3 + 2]]);
                            (tag.keep[t] as f32, 255.0 - tag.keep[t] as f32, u, v)
                        } else {
                            (255.0, 0.0, 0.0, 0.0)
                        };
                        if x < w && y < h {
                            su += u;
                            sv += v;
                            keep += k;
                            alpha += a;
                            n += 1.0;
                        }
                    }
                    let (u, v) = places(cy * cw + cx);
                    chroma[u] = (chroma[u] as f32 * keep / n / 255.0 + su / n + 128.0 * alpha / n / 255.0).round().clamp(0.0, 255.0) as u8;
                    chroma[v] = (chroma[v] as f32 * keep / n / 255.0 + sv / n + 128.0 * alpha / n / 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        _ => {}
    }
}

fn test_tag(x: usize, y: usize, width: usize, height: usize) -> Tag {
    let ink = (0..width * height * 3).map(|i| (i.wrapping_mul(37) >> 2) as u8).collect();
    let keep = (0..width * height).map(|i| ((i * 29) >> 1) as u8).collect();
    Tag::new(x, y, width, height, ink, keep)
}

fn yuv_frame(w: usize, h: usize, format: PixelFormat) -> Frame {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut data: Vec<u8> = (0..w * h).map(|i| (i * 7 >> 2) as u8).collect();
    if format == PixelFormat::Nv12 {
        data.extend((0..cw * ch * 2).map(|i| (i * 13 >> 1) as u8));
    } else {
        data.extend((0..cw * ch * 2).map(|i| (i * 11 >> 1) as u8));
    }
    Frame::new(w as u32, h as u32, format, data)
}

#[test]
fn cached_yuv_apply_matches_the_per_frame_one_byte_for_byte() {
    for format in [PixelFormat::I420, PixelFormat::Nv12] {
        for (w, h) in [(64usize, 48usize), (65, 49), (130, 70)] {
            for (x, y, tw, th) in [(0, 0, 16, 16), (1, 1, 17, 15), (w - 17, h - 15, 17, 15), (3, 5, w - 3, h - 5), (0, 0, w, h), (w - 1, h - 1, 1, 1)] {
                let tag = test_tag(x, y, tw, th);
                let (mut a, mut b) = (yuv_frame(w, h, format), yuv_frame(w, h, format));
                apply_reference(&mut a, &tag);
                // twice, so the second goes through what the first cached
                apply(&mut b, &tag);
                let mut c = yuv_frame(w, h, format);
                apply(&mut c, &tag);
                assert!(a.data == b.data && a.data == c.data, "{format:?} {w}x{h} tag {x},{y} {tw}x{th}");
            }
        }
    }
    // the same tag on a frame of another size works out its terms again
    let tag = test_tag(1, 1, 17, 15);
    for (w, h) in [(64usize, 48usize), (65, 49), (64, 48)] {
        let (mut a, mut b) = (yuv_frame(w, h, PixelFormat::I420), yuv_frame(w, h, PixelFormat::I420));
        apply_reference(&mut a, &tag);
        apply(&mut b, &tag);
        assert!(a.data == b.data, "{w}x{h}");
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_tag_apply() {
    for (w, h, tw, th) in [(1920usize, 1080usize, 640usize, 130usize), (3840, 2160, 1280, 260)] {
        let tag = test_tag(101, 801 % (h - th), tw, th);
        let mut frame = yuv_frame(w, h, PixelFormat::I420);
        let best = |f: &mut dyn FnMut()| (0..7).map(|_| {
            let start = std::time::Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1000.0
        }).fold(f64::MAX, f64::min);
        apply(&mut frame, &tag);
        let new = best(&mut || apply(&mut frame, &tag));
        let old = best(&mut || apply_reference(&mut frame, &tag));
        println!("tag apply {w}x{h} tile {tw}x{th}: old {old:.3} ms, new {new:.3} ms");
    }
}
