//! The category hues, measured: read from theme.slint, in OKLCH. Every card group's colour sits in a small lightness band, stays
//! under a chroma cap, clears 3:1 against the card, and keeps its hue away from the status colours (ok, warn, err), in the new
//! look and in Classic. A colour that fails a check is listed in KNOWN with why, so the test passes today and fails when a
//! colour changes: either a new failure, or a known one that is fixed and must come off the list.

use std::collections::BTreeSet;

/// The card groups, as Theme.plugin-hue names them.
const GROUPS: &[&str] = &["camera", "look", "vision", "preview", "on_screen", "sound", "streaming", "extras"];
const STATUS: &[&str] = &["ok", "warn", "err"];

/// A hue's lightness may stray this far from the middle one of the eight.
const LIGHTNESS_SPREAD: f64 = 0.04;
/// Chroma stays under this: loud enough to name a card, never neon.
const CHROMA_CAP: f64 = 0.14;
/// Against the card the hue clears this (WCAG's bar for a control's edge).
const CONTRAST_MIN: f64 = 3.0;
/// A hue is this many degrees from each status colour, at least. Greys (chroma under NEUTRAL) have no hue to compare.
const HUE_DISTANCE: f64 = 30.0;
const NEUTRAL: f64 = 0.04;

/// Checks that fail today, as "look/check/subject", each with what is wrong. Fix the colour and delete the line.
const KNOWN: &[(&str, &str)] = &[
    ("new/hue-distance/vision-ok", "Vision's green is the very colour of ok (#79bf8e): a lit Vision control reads as a success"),
    ("new/hue-distance/preview-warn", "Preview's orange is 9 degrees from warn, so a warning and a Preview outline look alike"),
    ("new/hue-distance/look-err", "Look's pink is 27 degrees from err, just short of the bar"),
    ("classic/hue-distance/camera-ok", "Camera's teal is 25 degrees from ok, just short of the bar"),
    ("classic/hue-distance/preview-warn", "Preview's peach is 22 degrees from warn"),
    ("classic/hue-distance/on_screen-err", "On screen's pink is 28 degrees from err, just short of the bar"),
    ("classic/hue-distance/sound-warn", "Sound's yellow is 15 degrees from warn, so a warning and a Sound outline look alike"),
    ("classic/lightness/sound", "Sound's yellow is lighter than the other hues (0.862 against a middle of 0.806)"),
];

#[derive(Clone, Copy, Debug)]
struct Oklch {
    l: f64,
    c: f64,
    h: f64,
}

fn parse(hex: &str) -> [u8; 3] {
    let n = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap();
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn luminance(rgb: [u8; 3]) -> f64 {
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Björn Ottosson's OKLab, in polar form.
fn oklch(rgb: [u8; 3]) -> Oklch {
    let (r, g, b) = (linear(rgb[0]), linear(rgb[1]), linear(rgb[2]));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    let lightness = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
    let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
    let b = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
    Oklch { l: lightness, c: a.hypot(b), h: b.atan2(a).to_degrees().rem_euclid(360.0) }
}

/// The `#rrggbb` values in a piece of Slint, in order.
fn hexes(text: &str) -> Vec<[u8; 3]> {
    text.split('#').skip(1).filter_map(|t| t.get(..6)).filter(|h| h.chars().all(|c| c.is_ascii_hexdigit())).map(parse).collect()
}

/// (new, classic) of a token written `classic ? #a : #b`, from the line that declares it.
fn token(theme: &str, name: &str) -> ([u8; 3], [u8; 3]) {
    let line = theme.lines().find(|l| l.contains(&format!("property <color> {name}:"))).unwrap_or_else(|| panic!("theme.slint has no {name}"));
    let found = hexes(line);
    assert_eq!(found.len(), 2, "{name} has a classic and a new value: {line}");
    (found[1], found[0])
}

/// (new, classic) of a card group's colour, from plugin-hue.
fn hue(theme: &str, group: &str) -> ([u8; 3], [u8; 3]) {
    let line = theme.lines().find(|l| l.contains(&format!("group == \"{group}\" ?"))).unwrap_or_else(|| panic!("plugin-hue has no {group}"));
    let found = hexes(line);
    assert!(found.len() >= 2, "{group} has a classic and a new value: {line}");
    (found[1], found[0])
}

fn theme_text() -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/theme.slint")).unwrap()
}

fn distance(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// Every check that fails, as "look/check/subject: what".
fn failures() -> Vec<(String, String)> {
    let theme = theme_text();
    let mut out = Vec::new();
    for (look, pick) in [("new", 0), ("classic", 1)] {
        let at = |(n, c): ([u8; 3], [u8; 3])| if pick == 0 { n } else { c };
        let card = at(token(&theme, "card"));
        let status: Vec<(&str, Oklch)> = STATUS.iter().map(|s| (*s, oklch(at(token(&theme, s))))).collect();
        let hues: Vec<(&str, [u8; 3])> = GROUPS.iter().map(|g| (*g, at(hue(&theme, g)))).collect();
        let mut lightness: Vec<f64> = hues.iter().map(|(_, rgb)| oklch(*rgb).l).collect();
        lightness.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let middle = (lightness[3] + lightness[4]) / 2.0;
        for (group, rgb) in &hues {
            let o = oklch(*rgb);
            let mut fail = |check: &str, subject: String, what: String| out.push((format!("{look}/{check}/{subject}"), what));
            if (o.l - middle).abs() > LIGHTNESS_SPREAD {
                fail("lightness", group.to_string(), format!("{group} is at lightness {:.3}, {:.3} from the middle one ({middle:.3}); allowed {LIGHTNESS_SPREAD}", o.l, (o.l - middle).abs()));
            }
            if o.c > CHROMA_CAP {
                fail("chroma", group.to_string(), format!("{group} has chroma {:.3}, over {CHROMA_CAP}", o.c));
            }
            let ratio = contrast(*rgb, card);
            if ratio < CONTRAST_MIN {
                fail("contrast", group.to_string(), format!("{group} is {ratio:.2}:1 against the card, under {CONTRAST_MIN}:1"));
            }
            if o.c >= NEUTRAL {
                for (name, s) in &status {
                    let d = distance(o.h, s.h);
                    if d < HUE_DISTANCE {
                        fail("hue-distance", format!("{group}-{name}"), format!("{group} (hue {:.0}) is {d:.0} degrees from {name} (hue {:.0}); wanted {HUE_DISTANCE}", o.h, s.h));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn category_hues_are_close_in_lightness_modest_in_chroma_readable_and_apart_from_status() {
    let found = failures();
    let known: BTreeSet<&str> = KNOWN.iter().map(|(id, _)| *id).collect();
    let seen: BTreeSet<&str> = found.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(known.len(), KNOWN.len(), "KNOWN lists a check twice");
    let new: Vec<String> = found.iter().filter(|(id, _)| !known.contains(id.as_str())).map(|(id, what)| format!("{id}: {what}")).collect();
    assert!(new.is_empty(), "a colour fails a check:\n{}", new.join("\n"));
    let fixed: Vec<&str> = known.iter().filter(|id| !seen.contains(**id)).copied().collect();
    assert!(fixed.is_empty(), "these now pass, so take them off KNOWN: {fixed:?}");
}

#[test]
fn the_conversion_agrees_with_published_values() {
    let white = oklch([255, 255, 255]);
    assert!((white.l - 1.0).abs() < 1e-3 && white.c < 1e-3, "{white:?}");
    let black = oklch([0, 0, 0]);
    assert!(black.l.abs() < 1e-6 && black.c < 1e-6, "{black:?}");
    // sRGB red in Ottosson's own table: L 0.628, C 0.258, hue 29.2
    let red = oklch([255, 0, 0]);
    assert!((red.l - 0.628).abs() < 2e-3 && (red.c - 0.258).abs() < 2e-3 && (red.h - 29.2).abs() < 0.3, "{red:?}");
    assert!((contrast([255, 255, 255], [0, 0, 0]) - 21.0).abs() < 1e-9);
}

#[test]
fn every_group_and_status_is_found_in_the_theme() {
    let theme = theme_text();
    for g in GROUPS {
        let (n, c) = hue(&theme, g);
        assert_ne!(n, c, "{g} has a value of its own in each look");
    }
    for s in STATUS {
        token(&theme, s);
    }
    token(&theme, "card");
}
