use super::*;

fn face(px: f32) -> Option<Face> {
    let face = app_font(Weight::Regular).map(|f| Face::em(f, px));
    if face.is_none() {
        eprintln!("no font on this machine: text measuring is skipped");
    }
    face
}

/// The face, when it or its fallbacks have a glyph for every character of the text.
fn face_with(px: f32, text: &str, what: &str) -> Option<Face> {
    let face = face(px)?;
    if text.chars().all(|c| face.has_glyph(c)) {
        return Some(face);
    }
    eprintln!("no font on this machine has every character of the {what}: skipped");
    None
}

/// DejaVu Sans, which the Qt numbers below were measured with (QFontMetricsF, hinting off like the name tag).
fn dejavu(px: f32) -> Option<Face> {
    let face = font_named(&["DejaVuSans.ttf"]).map(|f| Face::em(f, px));
    if face.is_none() {
        eprintln!("DejaVu Sans is not installed: the Qt comparison is skipped");
    }
    face
}

/// The glyph a character has on its own, which for Arabic is the isolated form.
fn nominal(face: &Face, ch: char) -> u16 {
    face.pick(ch).unwrap().glyph_id(ch).0
}

/// The pixels a line draws with at least this much coverage.
fn ink(face: &Face, text: &str) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    face.draw(&face.layout(text, 0.0), 50.0, 100.0, |x, y, c| {
        if c > 0.3 {
            out.push((x, y));
        }
    });
    out
}

#[test]
fn text_is_wider_the_more_of_it_and_the_bigger() {
    let Some(small) = face(20.0) else { return };
    let big = face(40.0).unwrap();
    assert!(small.advance("Luna Kozankiewicz") > small.advance("Luna"));
    let ratio = big.advance("Luna") / small.advance("Luna");
    assert!((ratio - 2.0).abs() < 0.02, "{ratio}");
    assert!(small.height() > 20.0 && small.ascent() > small.descent());
}

#[test]
fn a_long_line_is_cut_with_an_ellipsis_to_fit() {
    let Some(f) = face(30.0) else { return };
    let text = "Wolfeschlegelsteinhausenbergerdorff";
    let cut = f.elide(text, 200.0);
    assert!(cut.ends_with('\u{2026}') && f.advance(&cut) <= 200.0 && cut.len() < text.len());
    assert_eq!(f.elide("Luna", 200.0), "Luna");
}

#[test]
fn drawing_reports_pixels_inside_the_line() {
    let Some(f) = face(30.0) else { return };
    let line = f.layout("Hi", 0.0);
    let (mut hits, mut max_x) = (0, 0);
    f.draw(&line, 10.0, 40.0, |x, y, c| {
        if c > 0.5 {
            hits += 1;
            max_x = max_x.max(x);
            assert!(y > 40 - 35 && y < 40 + 2, "{y}");
        }
    });
    assert!(hits > 20 && max_x as f32 <= 10.0 + line.width + 1.0);
}

#[test]
fn direction_comes_from_the_first_strong_letter() {
    assert!(right_to_left("\u{633}\u{627}\u{631}\u{629}"));
    assert!(!right_to_left("Luna") && !right_to_left("\u{5c71}\u{7530}") && !right_to_left("   "));
    assert!(right_to_left("  \u{647}\u{64a}"));
}

#[test]
fn direction_follows_bidi_classes_not_script_ranges() {
    // Qt's drawText and name_tag.right_to_left both pick left to right for these (an Arabic comma and an Arabic-Indic digit are
    // not strong), checked by painting each with and without a left-to-right mark
    assert!(!right_to_left("\u{60c}ABC \u{5e9}\u{5dc}\u{5d5}\u{5dd}"));
    assert!(!right_to_left("\u{661}ABC \u{5e9}\u{5dc}\u{5d5}\u{5dd}"));
    assert!(right_to_left("123 \u{5e9}\u{5dc}\u{5d5}\u{5dd}"));
    let text = "\u{60c}ABC \u{5e9}\u{5dc}\u{5d5}\u{5dd}";
    let Some(f) = face_with(40.0, text, "Arabic comma, Latin and Hebrew") else { return };
    // read from the left: the comma, the Latin and the space as they are written, then the Hebrew from its right end
    assert_eq!(f.layout(text, 0.0).clusters(), [0, 2, 3, 4, 5, 12, 10, 8, 6]);
}

#[test]
fn pairs_of_letters_are_kerned() {
    let Some(f) = face(40.0) else { return };
    let (pair, apart) = (f.advance("AV"), f.advance("A") + f.advance("V"));
    assert!(pair < apart - 0.2, "AV is {pair}, A and V alone are {apart}");
}

#[test]
fn letters_are_spaced_apart_after_each_cluster() {
    // QFontMetricsF.horizontalAdvance with AbsoluteSpacing at 40 px in DejaVu Sans: spacing follows every cluster, the last too
    let Some(f) = dejavu(40.0) else { return };
    let grown = |text: &str, spacing: f32| f.layout(text, spacing).width - f.layout(text, 0.0).width;
    for (text, spacing, qt) in [
        ("Telescope", -1.5, -13.5),
        ("Telescope", 5.0, 45.0),
        ("abcd", 2.0, 8.0),
        ("abcd", -1.5, -6.0),
        ("a", 2.0, 2.0),
        // a mark rides on its base, so it takes no spacing of its own
        ("x\u{301}", 5.0, 5.0),
        ("x\u{301}y\u{301}", 2.0, 4.0),
        // lam and alef are one ligature, so three clusters
        ("\u{633}\u{644}\u{627}\u{645}", -1.5, -4.5),
        ("\u{644}\u{627}", 2.0, 2.0),
        ("", 5.0, 0.0),
    ] {
        let got = grown(text, spacing);
        assert!((got - qt).abs() < 0.01, "{text:?} spaced by {spacing}: {got}, Qt {qt}");
    }
}

#[test]
fn arabic_letters_join() {
    let word = "\u{633}\u{644}\u{627}\u{645}";
    let Some(f) = face_with(40.0, word, "Arabic word") else { return };
    let line = f.layout(word, 0.0);
    let own: Vec<u16> = word.chars().map(|c| nominal(&f, c)).collect();
    let joined = line.glyph_ids().iter().filter(|id| !own.contains(id)).count();
    // the seen takes its initial form and the lam and alef join into one glyph
    assert!(joined >= 2, "shaped {:?}, the letters alone are {own:?}", line.glyph_ids());
    assert_ne!(line.glyph_ids().len(), own.len());
    // read from the right, so the last letter is drawn first and the first one last
    assert_eq!((line.clusters()[0], *line.clusters().last().unwrap()), (6, 0));
    // three letters that all join, none in its own form
    let word = "\u{633}\u{644}\u{645}";
    let line = f.layout(word, 0.0);
    let own: Vec<u16> = word.chars().map(|c| nominal(&f, c)).collect();
    assert_eq!(line.clusters(), [4, 2, 0]);
    assert!(line.glyph_ids().iter().all(|id| !own.contains(id)), "shaped {:?}, the letters alone are {own:?}", line.glyph_ids());
    let apart: f32 = word.chars().map(|c| f.advance(&c.to_string())).sum();
    assert!((line.width - apart).abs() > 0.5, "joined letters are not the same width as separate ones");
}

#[test]
fn a_run_is_shaped_in_the_script_of_its_letters_not_of_the_run_before() {
    // Hebrew then Arabic: one right to left run, and when one fallback font covers both, one font piece too. Qt itemizes by script,
    // so the Arabic letters still join (probed: their glyphs are the initial, medial and final forms)
    let text = "\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{633}\u{644}\u{645}";
    let Some(f) = face_with(40.0, text, "Hebrew then Arabic") else { return };
    let own: Vec<u16> = "\u{633}\u{644}\u{645}".chars().map(|c| nominal(&f, c)).collect();
    let line = f.layout(text, 0.0);
    let arabic: Vec<u16> = line.clusters().iter().zip(line.glyph_ids()).filter(|(c, _)| **c >= 9).map(|(_, id)| id).collect();
    assert_eq!(arabic.len(), 3, "{:?}", line.clusters());
    assert!(arabic.iter().all(|id| !own.contains(id)), "the Arabic is shaped as {arabic:?}, the letters alone are {own:?}");
    // and the Arabic does not change how the Hebrew is shaped
    let hebrew = f.layout("\u{5e9}\u{5dc}\u{5d5}\u{5dd}", 0.0).glyph_ids();
    assert!(line.glyph_ids().ends_with(&hebrew));
}

#[test]
fn a_combining_mark_lands_over_its_base() {
    // there is no x with an acute in Unicode, so nothing composes it into one glyph
    let Some(f) = face_with(60.0, "x\u{301}", "x with an accent") else { return };
    let base = ink(&f, "x");
    let (left, right) = (base.iter().map(|p| p.0).min().unwrap(), base.iter().map(|p| p.0).max().unwrap());
    let top = base.iter().map(|p| p.1).min().unwrap();
    let above: Vec<i32> = ink(&f, "x\u{301}").into_iter().filter(|p| p.1 < top - 1).map(|p| p.0).collect();
    assert!(above.len() > 4, "no accent drawn above the x");
    let centre = above.iter().sum::<i32>() as f32 / above.len() as f32;
    assert!(centre > left as f32 && centre < right as f32, "the accent is centred at {centre}, the x spans {left} to {right}");
    assert!((f.advance("x\u{301}") - f.advance("x")).abs() < 0.01, "a mark adds no width");
}

#[test]
fn an_indic_vowel_sign_is_drawn_before_its_consonant() {
    let text = "\u{915}\u{93f}";
    let Some(f) = face_with(40.0, text, "Devanagari syllable") else { return };
    let ids = f.layout(text, 0.0).glyph_ids();
    assert!(ids.len() >= 2, "{ids:?}");
    // the sign for the vowel i is written after the letter and shown before it
    assert_ne!(ids[0], nominal(&f, '\u{915}'), "{ids:?}");
    assert_eq!(*ids.last().unwrap(), nominal(&f, '\u{915}'), "{ids:?}");
}

#[test]
fn a_right_to_left_line_keeps_its_latin_and_digits_in_reading_order() {
    let text = "\u{5e9}\u{5e8}\u{5d4} ABC 123";
    let Some(f) = face_with(40.0, text, "Hebrew name with Latin") else { return };
    // from the left: the Latin and the number as they read, then the Hebrew from its right end
    assert_eq!(f.layout(text, 0.0).clusters(), [7, 8, 9, 10, 11, 12, 13, 6, 4, 2, 0]);
    // and in a left to right line the number after the Hebrew comes first (it is the Hebrew that is turned around)
    let text = "ABC \u{5e9}\u{5e8}\u{5d4} 123";
    assert_eq!(f.layout(text, 0.0).clusters(), [0, 1, 2, 3, 11, 12, 13, 10, 8, 6, 4]);
}

#[test]
fn a_right_to_left_line_is_cut_at_its_left_end() {
    let text = "\u{633}\u{644}\u{627}\u{645} \u{639}\u{644}\u{64a}\u{643}\u{645} \u{648}\u{631}\u{62d}\u{645}\u{629} \u{627}\u{644}\u{644}\u{647}";
    let Some(f) = face_with(30.0, text, "Arabic phrase") else { return };
    let cut = f.elide(text, f.advance(text) * 0.6);
    assert!(cut.ends_with('\u{2026}') && text.starts_with(&cut[..cut.len() - 3]), "{cut}");
    // the end of the text is the left side, so the ellipsis is drawn first
    assert_eq!(f.layout(&cut, 0.0).clusters()[0], cut.len() - 3);
}

#[test]
fn a_line_is_never_cut_between_a_letter_and_its_mark() {
    let text = "x\u{301}".repeat(12);
    let Some(f) = face_with(30.0, &text, "accented letters") else { return };
    for width in (10..300).step_by(7) {
        let cut = f.elide(&text, width as f32);
        let kept = cut.trim_end_matches('\u{2026}').len();
        assert!(!text[kept..].starts_with('\u{301}'), "cut to {width}: {cut:?}");
    }
}

#[test]
fn elision_cuts_where_qt_does() {
    // QFontMetricsF.elidedText(ElideRight) at 40 px, DejaVu Sans, hinting off. Qt keeps what comes before the grapheme whose width
    // takes the sum past what is left after the ellipsis, so it can overshoot: "T\u{2026}" is 64.4 px wide in a width of 60
    let Some(f) = dejavu(40.0) else { return };
    for (width, want) in [(60.0, "T\u{2026}"), (80.0, "T\u{2026}"), (100.0, "Tel\u{2026}"), (120.0, "Tele\u{2026}"), (150.0, "Teles\u{2026}"), (200.0, "Telescope")] {
        assert_eq!(f.elide("Telescope", width), want, "width {width}");
    }
    let hebrew = "\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{5e2}\u{5d5}\u{5dc}\u{5dd}";
    for (width, want) in [(60.0, "\u{2026}"), (80.0, "\u{5e9}\u{2026}"), (100.0, "\u{5e9}\u{5dc}\u{2026}"), (120.0, "\u{5e9}\u{5dc}\u{5d5}\u{2026}"), (150.0, "\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{2026}")] {
        assert_eq!(f.elide(hebrew, width), want, "width {width}");
    }
    assert_eq!(f.elide("a\u{1F600}b", 80.0), "a\u{2026}");
    assert_eq!(f.elide("x\u{301}y\u{301}z\u{301}", 60.0), "\u{2026}");
}

#[test]
fn an_arabic_line_is_cut_with_a_joiner_so_the_last_letter_keeps_its_joining_form() {
    // QFontMetricsF.elidedText at 40 px, DejaVu Sans: Qt puts a zero width joiner before the ellipsis when the first letter cut off
    // would have joined to the one before (even across a space)
    let Some(f) = dejavu(40.0) else { return };
    let arabic = "\u{633}\u{644}\u{627}\u{645} \u{639}\u{644}\u{64a}\u{643}\u{645}";
    for (width, want) in [
        (40.0, "\u{200d}\u{2026}"),
        (60.0, "\u{200d}\u{2026}"),
        (80.0, "\u{633}\u{200d}\u{2026}"),
        (100.0, "\u{633}\u{644}\u{627}\u{200d}\u{2026}"),
        (120.0, "\u{633}\u{644}\u{627}\u{200d}\u{2026}"),
        (150.0, "\u{633}\u{644}\u{627}\u{645} \u{200d}\u{2026}"),
    ] {
        assert_eq!(f.elide(arabic, width), want, "width {width}");
    }
    // the letter before the ellipsis is drawn in its initial form, not on its own
    let cut = f.elide(arabic, 80.0);
    let seen = *f.layout(&cut, 0.0).glyph_ids().last().unwrap();
    assert_ne!(seen, nominal(&f, '\u{633}'), "the seen is drawn as if it stood alone");
    // Hebrew and Latin do not join, so they get none
    assert_eq!(f.elide("\u{5e9}\u{5dc}\u{5d5}\u{5dd}\u{5dd}\u{5dd}", 100.0), "\u{5e9}\u{5dc}\u{2026}");
}

#[test]
fn a_line_with_no_room_for_the_ellipsis_is_dropped() {
    // QFontMetricsF.elidedText at 40 px: the ellipsis is 40 px wide, so under that nothing is left, not even the ellipsis
    let Some(f) = dejavu(40.0) else { return };
    for (width, want) in [(-5.0, ""), (0.0, ""), (10.0, ""), (39.0, ""), (40.0, "\u{2026}"), (41.0, "\u{2026}")] {
        assert_eq!(f.elide("Telescope", width), want, "width {width}");
    }
    // a text of one UTF-16 unit is never cut, however narrow
    assert_eq!(f.elide("W", 10.0), "W");
    assert_eq!(f.elide("WW", 39.0), "");
    // a character outside the BMP is two units, so it is cut like any other text
    assert_eq!(f.elide("\u{1F600}", 10.0), "");
}
