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
        ("audio/ui/microphone.slint", 3),
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
fn rows_that_come_and_go_unfold_in_the_cards_that_were_swept() {
    // `if state: Row` pops in: these cards use Unfolding for what their state shows and hides
    for rel in ["effects/ui/background.slint", "effects/ui/fun_pack.slint", "effects/ui/colour_grading.slint"] {
        let text = std::fs::read_to_string(crates().join(rel)).unwrap();
        assert!(text.contains("Unfolding {"), "{rel} unfolds its conditional rows");
        for (n, line) in lines(&text) {
            let l = line.trim_start();
            assert!(!(l.starts_with("if ") && (l.contains(": RailSlider") || l.contains(": RailRow"))) || l.contains("vision") || l.contains("status"), "{rel}:{n}: {l} pops in");
        }
    }
}
