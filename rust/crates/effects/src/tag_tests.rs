use super::*;
use crate::pixels::gaussian_blur;

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
fn a_gaussian_blur_spreads_a_dot_and_keeps_its_weight() {
    let mut src = vec![0u8; 21 * 21];
    src[10 * 21 + 10] = 255;
    let out = gaussian_blur(&src, 21, 21, 2.0);
    assert!(out[10 * 21 + 10] < 255 && out[10 * 21 + 10] > out[10 * 21 + 12] && out[10 * 21 + 12] > 0);
    let total: u32 = out.iter().map(|v| *v as u32).sum();
    assert!((total as f32 - 255.0).abs() < 30.0, "{total}");
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
