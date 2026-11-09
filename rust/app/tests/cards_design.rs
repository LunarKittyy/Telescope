// Rules of the redesign that a later card could break without anyone noticing, checked on the cards' own .slint files (the kit has its own
// in ui-kit/tests/design.rs). The Settings window's pages are left out: they are redesigned apart from the cards.
use std::path::{Path, PathBuf};

fn crates() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates")
}

/// Pieces of the Settings window, which follow their own rules.
const SETTINGS: &[&str] = &[
    "shell/ui/settings.slint", "setup/ui/", "system/ui/updates.slint", "system/ui/mcp.slint", "system/ui/shortcuts.slint", "system/ui/startup.slint",
    "outputs/ui/wait_screen.slint", "effects/ui/on_air.slint", "link/ui/browser_camera.slint", "shell/ui/specimen.slint",
];

fn cards() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !["target", "ui-kit", "tests", "plugin-host"].contains(&name.as_str()) {
                    walk(&path, out);
                }
            } else if name.ends_with(".slint") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&crates(), &mut files);
    files.into_iter()
        .map(|f| (f.strip_prefix(crates()).unwrap().to_string_lossy().replace('\\', "/"), std::fs::read_to_string(&f).unwrap()))
        .filter(|(rel, _)| !SETTINGS.iter().any(|s| rel.starts_with(s) || rel == s))
        .collect()
}

fn lines<'a>(text: &'a str) -> impl Iterator<Item = (usize, &'a str)> {
    text.lines().enumerate().map(|(i, l)| (i + 1, l)).filter(|(_, l)| !l.trim_start().starts_with("//"))
}

fn report(what: &str, problems: Vec<String>) {
    assert!(problems.is_empty(), "{what}:\n{}", problems.join("\n"));
}

#[test]
fn cards_use_the_tokens_not_their_old_names() {
    const OLD: [&str; 13] = ["bg", "surface", "surface-raise", "surface-hover", "surface-sunk", "border", "border-strong", "border-hover", "text", "text-dim", "text-faint", "text-disabled", "text-hover"];
    let mut problems = Vec::new();
    for (rel, text) in cards() {
        for (n, line) in lines(&text) {
            for old in OLD {
                let key = format!("Theme.{old}");
                if let Some(at) = line.find(&key) {
                    if !line[at + key.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '-') {
                        problems.push(format!("{rel}:{n}: {key}"));
                    }
                }
            }
        }
    }
    report("old token names", problems);
}

#[test]
fn a_card_fades_itself_when_idle_and_has_no_buttons_without_a_surface() {
    let mut problems = Vec::new();
    for (rel, text) in cards() {
        for (n, line) in lines(&text) {
            if line.contains("opacity: active ? 1 : 0.4") || line.contains("opacity: root.active ? 1 : 0.4") {
                problems.push(format!("{rel}:{n}: a card says `idle: !active`, the kit fades it both ways"));
            }
            if line.contains("role: \"quiet\"") || line.contains("strip: true") || line.contains("quiet-when-disabled") {
                problems.push(format!("{rel}:{n}: a button with no surface at rest"));
            }
        }
    }
    report("cards", problems);
}

#[test]
fn a_status_is_a_status_not_a_paragraph() {
    // The few places a sentence is the point: a question a dialog asks before it does something, or a licence line
    const SENTENCES: &[(&str, usize)] = &[
        ("link/ui/connection.slint", 4),
        ("effects/ui/composite_editor.slint", 2),
    ];
    let mut problems = Vec::new();
    for (rel, text) in cards() {
        let notes = lines(&text).filter(|(_, l)| l.trim_start().starts_with("Note {") || l.contains(": Note {") || l.contains("if ") && l.contains("Note {")).count();
        let allowed = SENTENCES.iter().find(|(f, _)| *f == rel).map_or(0, |(_, n)| *n);
        if notes > allowed {
            problems.push(format!("{rel} puts {notes} sentences in the window (allowed {allowed}): use a Status with the sentence as its detail"));
        }
    }
    report("sentences in cards", problems);
}

#[test]
fn rows_that_come_and_go_unfold_in_every_card() {
    // `if state: Row` pops in. A card's own rows (the first level inside it, four spaces in) come and go through Unfolding, a status that
    // shuts keeps its last words (StatusUnfold, VisionStatus), and the Settings pages are left out
    let mut problems = Vec::new();
    let mut swept = 0;
    for (rel, text) in cards() {
        if !rel.contains("/ui/") || rel.contains("composite_editor") || rel.contains("plugin-host") {
            continue;
        }
        if text.contains("export component") && text.contains("inherits Card") {
            swept += 1;
        }
        for (n, line) in lines(&text) {
            let Some(rest) = line.strip_prefix("    if ") else { continue };
            if line.starts_with("     ") {
                continue;
            }
            if ["RailRow", "RailSlider", "SwitchRow", "VisionStatus", "StatusLine", "Button", "Note"].iter().any(|k| rest.contains(&format!(": {k} {{"))) {
                problems.push(format!("{rel}:{n}: {} pops in: put it in an UnfoldGroup with an Unfolding", line.trim()));
            }
        }
    }
    assert!(swept > 15, "found only {swept} cards");
    report("rows that pop in", problems);
}

#[test]
fn sizes_are_tokens_not_pixels() {
    // Theme.u(n), row, gap, pad, r-ctl, body and the rest. What stays: 0px, a hairline of 1px or half a pixel, the 1px a length is divided or multiplied by to
    // make a number, the lines of a drawn path (stroke widths, viewboxes), and the preview's own marks, whose window has a Theme of its own
    const OWN_THEME: &[&str] = &["shell/ui/preview.slint"];
    let mut problems = Vec::new();
    for (rel, text) in cards() {
        if OWN_THEME.contains(&rel.as_str()) || !rel.contains("/ui/") || rel.contains("plugin-host") {
            continue;
        }
        for (n, line) in lines(&text) {
            if ["stroke-width", "viewbox", "commands"].iter().any(|k| line.contains(k)) {
                continue;
            }
            let bytes = line.as_bytes();
            for (at, _) in line.match_indices("px") {
                let start = line[..at].rfind(|c: char| !(c.is_ascii_digit() || c == '.')).map_or(0, |i| i + 1);
                let number = &line[start..at];
                if number.is_empty() || start > 0 && (bytes[start - 1].is_ascii_alphabetic() || bytes[start - 1] == b'-' && start > 1 && bytes[start - 2].is_ascii_alphabetic()) {
                    continue;
                }
                let before = line[..start].trim_end();
                if number == "0" || number == "1" || number == "0.5" || ["1000", "10000", "100000"].contains(&number) || (before.ends_with('/') || before.ends_with('*')) && number == "1" {
                    continue;
                }
                problems.push(format!("{rel}:{n}: {number}px in `{}`", line.trim()));
                break;
            }
        }
    }
    report("pixel sizes in cards", problems);
}
