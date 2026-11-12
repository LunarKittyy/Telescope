//! The category hues, measured: read from theme.slint, in OKLCH. Every card group's colour sits in a small lightness band, stays
//! under a chroma cap, clears 3:1 against the card, and keeps its hue away from the status colours (ok, warn, err), in the new
//! look, in Classic, in OLED and in Light. A colour that fails a check is listed in KNOWN with why, so the test passes today and fails when a
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
    ("dark/hue-distance/vision-ok", "Vision's green is the very colour of ok (#79bf8e): a lit Vision control reads as a success"),
    ("dark/hue-distance/preview-warn", "Preview's orange is 9 degrees from warn, so a warning and a Preview outline look alike"),
    ("dark/hue-distance/look-err", "Look's pink is 27 degrees from err, just short of the bar"),
    ("classic/hue-distance/camera-ok", "Camera's teal is 25 degrees from ok, just short of the bar"),
    ("classic/hue-distance/preview-warn", "Preview's peach is 22 degrees from warn"),
    ("classic/hue-distance/on_screen-err", "On screen's pink is 28 degrees from err, just short of the bar"),
    ("classic/hue-distance/sound-warn", "Sound's yellow is 15 degrees from warn, so a warning and a Sound outline look alike"),
    ("classic/lightness/sound", "Sound's yellow is lighter than the other hues (0.862 against a middle of 0.806)"),
    ("oled/hue-distance/vision-ok", "OLED keeps the dark look's hues: Vision's green is the very colour of ok"),
    ("oled/hue-distance/preview-warn", "OLED keeps the dark look's hues: Preview's orange is 9 degrees from warn"),
    ("oled/hue-distance/look-err", "OLED keeps the dark look's hues: Look's pink is 27 degrees from err"),
    ("light/hue-distance/vision-ok", "Light darkens the dark look's hues and keeps their angles: Vision's green is the very colour of ok"),
    ("light/hue-distance/preview-warn", "Light keeps the dark look's hue angles: Preview's orange is 9 degrees from warn"),
    ("light/hue-distance/look-err", "Light keeps the dark look's hue angles: Look's pink is 27 degrees from err"),
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

/// The four looks, in the order Theme.pick takes its values.
const LOOKS: [&str; 4] = ["dark", "classic", "oled", "light"];

/// The arguments of the first `pick(` after `from` in `line`, split at the commas that are not inside a bracket.
fn pick_args(line: &str, from: usize) -> Vec<String> {
    let start = line[from..].find("pick(").unwrap_or_else(|| panic!("no pick( in {line}")) + from + 5;
    let (mut depth, mut args, mut current) = (0, Vec::new(), String::new());
    for c in line[start..].chars() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                args.push(std::mem::take(&mut current).trim().to_string());
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    args.push(current.trim().to_string());
    args
}

/// A plain `#rrggbb`, or None for anything computed.
fn plain(text: &str) -> Option<[u8; 3]> {
    (text.len() == 7 && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())).then(|| parse(text))
}

/// The value of a token in each look (dark, classic, oled, light), from the line that declares it; None where it is computed.
fn token(theme: &str, name: &str) -> [Option<[u8; 3]>; 4] {
    let line = theme.lines().find(|l| l.contains(&format!("property <color> {name}:"))).unwrap_or_else(|| panic!("theme.slint has no {name}"));
    let args = pick_args(line, 0);
    assert_eq!(args.len(), 4, "{name} has a value per look: {line}");
    [0, 1, 2, 3].map(|i| plain(&args[i]))
}

/// A token that is a plain colour in this look.
fn colour(theme: &str, name: &str, look: usize) -> [u8; 3] {
    token(theme, name)[look].unwrap_or_else(|| panic!("{name} isn't a plain colour in {}", LOOKS[look]))
}

/// A card group's colour in each look, from plugin-hue.
fn hue(theme: &str, group: &str) -> [[u8; 3]; 4] {
    let line = theme.lines().find(|l| l.contains(&format!("group == \"{group}\" ?"))).unwrap_or_else(|| panic!("plugin-hue has no {group}"));
    let args = pick_args(line, line.find(&format!("group == \"{group}\"")).unwrap());
    assert_eq!(args.len(), 4, "{group} has a value per look: {line}");
    [0, 1, 2, 3].map(|i| plain(&args[i]).unwrap_or_else(|| panic!("{group} in {} is {}", LOOKS[i], args[i])))
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
    for (index, look) in LOOKS.iter().enumerate() {
        let card = colour(&theme, "card", index);
        let status: Vec<(&str, Oklch)> = STATUS.iter().map(|s| (*s, oklch(colour(&theme, s, index)))).collect();
        let hues: Vec<(&str, [u8; 3])> = GROUPS.iter().map(|g| (*g, hue(&theme, g)[index])).collect();
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
            // OLED's card is black and Light's is white: the hue is checked against the surface it sits on, which is the card
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
        let values = hue(&theme, g);
        assert_ne!(values[0], values[1], "{g} has a value of its own in Classic");
        assert_ne!(values[0], values[3], "{g} has a value of its own in Light");
        assert_eq!(values[0], values[2], "{g} is the same in OLED as in Dark");
    }
    for s in STATUS {
        token(&theme, s);
    }
    token(&theme, "card");
}

/// WCAG's bar for text.
const TEXT_CONTRAST: f64 = 4.5;

/// `a` mixed `amount` of the way towards `b`: Slint's `a.mix(b, f)` keeps f of `a`, so a button's `hue.mix(white, 0.85)` is `mixed(hue, white, 0.15)`.
fn mixed(a: [u8; 3], b: [u8; 3], amount: f64) -> [u8; 3] {
    let m = |x: u8, y: u8| (f64::from(x) * (1.0 - amount) + f64::from(y) * amount).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// The looks whose primary button in a card wears the card's colour (Classic's stays violet).
const HUE_LOOKS: [usize; 3] = [0, 2, 3];

#[test]
fn text_on_a_primary_button_in_a_card_reads_on_every_group_colour_resting_hovered_and_pressed() {
    let theme = theme_text();
    let hover_to = |look: usize| if look == 3 { [0; 3] } else { [255; 3] };
    for look in HUE_LOOKS {
        let ink = colour(&theme, "on-hue", look);
        for group in GROUPS {
            let rest = hue(&theme, group)[look];
            // The button moves 15% towards Theme.hue-hover-to on hover (lighter, but darker on Light) and 15% towards black when pressed
            for (state, fill) in [("rest", rest), ("hover", mixed(rest, hover_to(look), 0.15)), ("pressed", mixed(rest, [0; 3], 0.15))] {
                let ratio = contrast(ink, fill);
                assert!(ratio >= TEXT_CONTRAST, "the text on {group}'s primary button ({state}, {}) is {ratio:.2}:1, under {TEXT_CONTRAST}:1", LOOKS[look]);
            }
        }
    }
}

#[test]
fn start_and_stop_streaming_read_in_every_look_but_classic_resting_hovered_and_pressed() {
    let theme = theme_text();
    for look in HUE_LOOKS {
        let at = |name: &str| colour(&theme, name, look);
        let go = at("go-ink");
        for state in ["go-fill", "go-hover", "go-press"] {
            let ratio = contrast(go, at(state));
            assert!(ratio >= 7.0, "Start's text on {state} ({}) is {ratio:.2}:1, under 7:1", LOOKS[look]);
        }
        // Start's three steps are told apart: lighter than itself on hover and darker pressed in the dark looks, the other way up on Light
        let (fill, hover, press) = (luminance(at("go-fill")), luminance(at("go-hover")), luminance(at("go-press")));
        if look == 3 {
            assert!(press < fill && fill < hover, "Start's steps on Light");
        } else {
            assert!(fill > hover && hover > press, "Start's steps in {}", LOOKS[look]);
        }
        let stop = at("stop-ink");
        for state in ["stop-fill", "stop-hover", "stop-press"] {
            let ratio = contrast(stop, at(state));
            assert!(ratio >= 7.0, "Stop's text on {state} ({}) is {ratio:.2}:1, under 7:1", LOOKS[look]);
        }
        // The outline stands out from the fill and from the card at every step
        let edge = at("stop-edge");
        assert!(contrast(edge, at("stop-press")) >= 4.5 && contrast(edge, at("stop-fill")) >= 7.0 && contrast(edge, at("card")) >= CONTRAST_MIN, "Stop's outline ({})", LOOKS[look]);
        let (fill, hover, press) = (luminance(at("stop-fill")), luminance(at("stop-hover")), luminance(at("stop-press")));
        if look == 3 {
            assert!(fill > hover && hover > press, "Stop's steps on Light");
        } else {
            assert!(fill < hover && hover < press, "Stop's steps in {}", LOOKS[look]);
        }
    }
}

#[test]
fn the_text_greys_read_on_every_surface_in_every_look() {
    let theme = theme_text();
    // Classic keeps 5.0's greys, which were never held to this bar (its faint is 4.2:1 on a raised control)
    for (index, look) in LOOKS.iter().enumerate().filter(|(i, _)| *i != 1) {
        for text in ["ink", "ink2", "faint", "dim"] {
            for surface in ["card", "raise", "hover", "ground", "bubble"] {
                let (t, s) = (colour(&theme, text, index), colour(&theme, surface, index));
                let ratio = contrast(t, s);
                assert!(ratio >= TEXT_CONTRAST, "{text} on {surface} in {look} is {ratio:.2}:1, under {TEXT_CONTRAST}:1");
            }
        }
        // Secondary text stays near the primary's lightness: hierarchy comes from size and weight, not from grey
        let (ink, faint) = (oklch(colour(&theme, "ink", index)).l, oklch(colour(&theme, "faint", index)).l);
        assert!((ink - faint).abs() < 0.25, "faint text in {look} is far from ink: {ink:.2} against {faint:.2}");
    }
}

#[test]
fn the_neutral_looks_have_greys_with_no_tint() {
    let theme = theme_text();
    let names = ["stage", "chrome", "ground", "trough", "card", "raise", "hover", "sel", "press", "bubble", "line", "line-strong", "line-hover", "ink", "ink2", "faint", "dis", "dim", "hot"];
    for look in [0, 2, 3] {
        for name in names {
            let [r, g, b] = colour(&theme, name, look);
            assert!(r == g && g == b, "{name} in {} is tinted: {r} {g} {b}", LOOKS[look]);
        }
    }
}

#[test]
fn oled_is_black_where_the_dark_look_layers_and_light_layers_the_other_way_up() {
    let theme = theme_text();
    for name in ["stage", "chrome", "ground", "trough", "card", "raise", "bubble"] {
        assert_eq!(colour(&theme, name, 2), [0, 0, 0], "{name} in OLED is true black");
    }
    let lum = |name: &str, look: usize| luminance(colour(&theme, name, look));
    // The fills may still step a little, so the steps stay in order, but the telling is the outline's
    assert!(lum("raise", 2) < lum("hover", 2) && lum("hover", 2) < lum("sel", 2) && lum("sel", 2) < lum("press", 2), "OLED's fill steps rise");
    assert!(lum("hover", 2) < 0.02 && lum("press", 2) < 0.03, "OLED's fills stay near black");
    // Light: the window is a step off the cards, and a control rises off its card the way it does in the dark look, mirrored
    assert!(lum("card", 3) > lum("ground", 3) && lum("ground", 3) > lum("stage", 3), "Light's window sits between its cards and its stage");
    assert!(lum("raise", 3) > lum("hover", 3) && lum("hover", 3) > lum("press", 3), "Light's hover and press go darker, as the dark look's go lighter");
    assert!(lum("raise", 0) < lum("hover", 0) && lum("hover", 0) < lum("press", 0));
    // In OLED the outline tells the states apart: dimmest at rest, brighter hovered, brightest held, all readable against the card (3:1)
    let (edge, over, held) = (colour(&theme, "outline", 2), colour(&theme, "outline-hover", 2), colour(&theme, "outline-press", 2));
    assert!(luminance(edge) < luminance(over) && luminance(over) < luminance(held), "OLED's outline brightens with the pointer");
    for (name, c) in [("outline", edge), ("outline-hover", over), ("outline-press", held), ("pop-outline", colour(&theme, "pop-outline", 2))] {
        assert!(contrast(c, colour(&theme, "card", 2)) >= if name == "outline" { 3.0 } else { 4.0 }, "{name} against OLED's black");
    }
    // A card's outline and a control's differ, so a control inside a card doesn't merge with the card's edge
    assert!(luminance(colour(&theme, "card-outline", 2)) < luminance(edge));
    // The other looks have no outline tokens to draw: the fill does the telling
    let line = |name: &str| theme.lines().find(|l| l.contains(&format!("property <")) && l.contains(&format!(" {name}:"))).unwrap().to_string();
    for name in ["outline", "outline-hover", "outline-press", "card-outline", "pop-outline", "dis-outline"] {
        let args = pick_args(&line(name), 0);
        assert!(args[0] == "transparent" && args[1] == "transparent" && args[3] == "transparent" && args[2] != "transparent", "{name} is transparent outside OLED: {}", line(name));
    }
    for name in ["outline-w", "outline-w-over", "card-outline-w", "pop-outline-w"] {
        assert!(line(name).contains("look == 2 ?") && line(name).contains(": 0px"), "{name} is 0 outside OLED: {}", line(name));
    }
}
