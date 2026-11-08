//! STYLE.md's rules, checked: every kit widget says its kind, and cards outside the kit don't draw controls of their own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn kit() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn crates() -> PathBuf {
    kit().join("..")
}

/// The kinds STYLE.md lists in its table: the backticked name that starts each row.
fn listed_kinds() -> Vec<String> {
    let guide = std::fs::read_to_string(kit().join("STYLE.md")).expect("STYLE.md");
    let table = guide.split("## Control kinds").nth(1).expect("a Control kinds section");
    let table = table.split("\n## ").next().unwrap();
    table.lines().filter_map(|l| l.strip_prefix("| `")).filter_map(|l| l.split('`').next()).map(str::to_string).collect()
}

fn slint_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if name != "target" && name != "ui-kit" {
                slint_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "slint") {
            out.push(p);
        }
    }
}

#[test]
fn every_exported_widget_says_its_kind() {
    let kinds = listed_kinds();
    assert!(kinds.len() > 10, "STYLE.md lists its kinds: {kinds:?}");
    let mut missing = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    let mut files: Vec<_> = std::fs::read_dir(kit().join("ui/widgets")).unwrap().flatten().map(|e| e.path()).collect();
    files.sort();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        // Split at every top-level declaration; an exported component's block runs to the next one
        let mut blocks: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let starts = line.starts_with("export ") || line.starts_with("component ") || line.starts_with("global ") || line.starts_with("struct ");
            if starts {
                blocks.push((line.to_string(), String::new()));
            }
            if let Some(b) = blocks.last_mut() {
                b.1.push_str(line);
                b.1.push('\n');
            }
        }
        for (head, body) in blocks {
            let Some(rest) = head.strip_prefix("export component ") else { continue };
            let name = rest.split(|c: char| !c.is_alphanumeric()).next().unwrap();
            match body.split("control-kind: \"").nth(1).and_then(|k| k.split('"').next()) {
                Some(kind) if kinds.iter().any(|k| k == kind) => {
                    used.insert(kind.to_string());
                }
                Some(kind) => missing.push(format!("{}: {name} says kind \"{kind}\", which STYLE.md doesn't list", file.display())),
                None => missing.push(format!("{}: {name} has no control-kind", file.display())),
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    let unused: Vec<_> = kinds.iter().filter(|k| !used.contains(*k)).collect();
    assert!(unused.is_empty(), "STYLE.md lists kinds no widget says: {unused:?}");
}

/// Where a card still draws its own control (a bordered Rectangle or a TouchArea), by file: (borders, touch areas). These
/// are what's left to move into the kit, one card at a time; the numbers may only go down. A new file isn't on the list.
const OWN_CONTROLS: &[(&str, usize, usize)] = &[
    ("audio/ui/microphone.slint", 0, 1),
    ("effects/ui/composite_editor.slint", 8, 3),
    ("effects/ui/parts.slint", 2, 1),
    ("effects/ui/qr_reader.slint", 0, 1),
    ("setup/ui/parts.slint", 1, 2),
    ("setup/ui/setup.slint", 0, 1),
    ("shell/ui/cards/camera.slint", 1, 1),
    ("shell/ui/cards/onboarding.slint", 1, 0),
    ("shell/ui/cards/presets.slint", 1, 1),
    ("shell/ui/cards/preview.slint", 1, 1),
    ("shell/ui/frame.slint", 1, 1),
    ("shell/ui/preview.slint", 2, 1),
    ("shell/ui/settings.slint", 2, 6),
    ("system/ui/mcp.slint", 2, 1),
    ("system/ui/shortcuts.slint", 3, 2),
    ("system/ui/updates.slint", 0, 1),
];

/// (lines that give a Rectangle a border, TouchArea elements) in a card's .slint file.
fn own_controls(text: &str) -> (usize, usize) {
    let borders = text.lines().filter(|l| l.split_once("border-width:").is_some_and(|(_, v)| !v.trim_start().starts_with('0'))).count();
    let touches = text.matches("TouchArea {").count();
    (borders, touches)
}

#[test]
fn cards_outside_the_kit_draw_no_controls_of_their_own() {
    let mut files = Vec::new();
    slint_files(&crates(), &mut files);
    assert!(files.len() > 20, "found the cards' files");
    let allowed: BTreeMap<&str, (usize, usize)> = OWN_CONTROLS.iter().map(|(f, b, t)| (*f, (*b, *t))).collect();
    let mut problems = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for file in files {
        let rel = file.strip_prefix(crates()).unwrap().to_string_lossy().replace('\\', "/");
        let (borders, touches) = own_controls(&std::fs::read_to_string(&file).unwrap());
        seen.insert(rel.clone());
        let (b, t) = allowed.get(rel.as_str()).copied().unwrap_or((0, 0));
        if borders > b || touches > t {
            problems.push(format!("{rel} draws {borders} bordered boxes and {touches} touch areas of its own (allowed {b} and {t}): use a kit widget or add a kind to the kit"));
        }
        if borders < b || touches < t {
            problems.push(format!("{rel} is down to {borders} and {touches}: lower its numbers in OWN_CONTROLS (was {b} and {t})"));
        }
    }
    for f in allowed.keys() {
        assert!(seen.contains(*f), "OWN_CONTROLS names {f}, which doesn't exist");
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The fills STYLE.md lists under Sliders: the backticked names before the colon of each bullet.
fn listed_fills() -> Vec<String> {
    let guide = std::fs::read_to_string(kit().join("STYLE.md")).expect("STYLE.md");
    let section = guide.split("## Sliders").nth(1).expect("a Sliders section");
    let section = section.split("\n## ").next().unwrap();
    let mut out = Vec::new();
    for line in section.lines().filter(|l| l.starts_with("- `")) {
        let head = line.split(':').next().unwrap();
        out.extend(head.split('`').skip(1).step_by(2).filter(|n| n.chars().all(|c| c.is_ascii_lowercase())).map(str::to_string));
    }
    out
}

#[test]
fn cards_name_a_listed_fill_and_every_listed_fill_has_a_look() {
    let fills = listed_fills();
    assert!(fills.len() >= 11, "STYLE.md lists the fills: {fills:?}");
    let ramps = std::fs::read_to_string(kit().join("ui/widgets/fills.slint")).unwrap();
    for f in &fills {
        // plain is the card's colour, gradient and vibrance have their own stops, contrast its own component, the rest are greys
        let known = matches!(f.as_str(), "plain" | "gradient" | "vibrance" | "contrast") || ramps.contains(&format!("kind == \"{f}\""));
        assert!(known, "STYLE.md lists the fill {f}, which fills.slint doesn't draw");
    }
    let mut files = Vec::new();
    slint_files(&crates(), &mut files);
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for part in text.split("fill:").skip(1) {
            // The strings in the value, as in `fill: i == 0 ? "gradient" : "plain";`
            let value = part.split(';').next().unwrap();
            for name in value.split('"').skip(1).step_by(2) {
                assert!(fills.iter().any(|f| f == name), "{} asks for the fill {name}, which STYLE.md doesn't list", file.display());
            }
        }
    }
}

#[test]
fn the_bundled_fonts_are_there_with_their_licence() {
    let theme = std::fs::read_to_string(kit().join("ui/theme.slint")).unwrap();
    let imports: Vec<&str> = theme.lines().filter_map(|l| l.strip_prefix("import \"fonts/")).filter_map(|l| l.split('"').next()).collect();
    assert_eq!(imports.len(), 4, "regular, medium, semibold and bold: {imports:?}");
    for f in imports {
        assert!(kit().join("ui/fonts").join(f).metadata().is_ok_and(|m| m.len() > 100_000), "{f} is bundled");
    }
    assert!(std::fs::read_to_string(kit().join("ui/fonts/OFL.txt")).unwrap().contains("SIL OPEN FONT LICENSE Version 1.1"));
}

/// The text between the braces that open at `open` (the index of a `{`) and close with it.
fn braced(text: &str, open: usize) -> &str {
    let mut depth = 0;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    &text[open..]
}

#[test]
fn a_lone_switch_in_a_rail_row_is_a_switch_row_that_carries_its_label() {
    let mut files = Vec::new();
    slint_files(&crates(), &mut files);
    let mut problems = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (at, _) in text.match_indices("RailRow {") {
            let body = braced(&text, at + "RailRow ".len());
            let grouped = body.contains("HorizontalLayout") || body.contains("GridLayout") || body.contains("VerticalLayout");
            if body.matches("Switch {").count() == 1 && !grouped {
                problems.push(format!("{}: a RailRow holds one Switch on its own: use a SwitchRow, which carries the name beside the icon", file.display()));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn a_lone_on_off_setting_is_a_switch_row_not_a_toggle_tile() {
    let mut files = Vec::new();
    slint_files(&crates(), &mut files);
    let mut problems = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (at, _) in text.match_indices("RailRow {") {
            let body = braced(&text, at + "RailRow ".len());
            let tiles = body.matches("ToggleTile {").count();
            let grouped = body.contains("HorizontalLayout") || body.contains("GridLayout") || body.contains("VerticalLayout") || body.contains("Tiles {");
            if tiles == 1 && !grouped {
                problems.push(format!("{}: a RailRow holds one ToggleTile on its own: use a Switch in the row (ToggleTiles are for groups)", file.display()));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Files outside the kit that may use the filled violet (`Theme.fill`): a step's done mark, which is a status and not a selection.
const FILLED: &[&str] = &["shell/ui/cards/onboarding.slint"];

#[test]
fn no_card_marks_a_selection_with_a_fill() {
    // A picked option is the thick outline in the card's colour over a muted inside (the kit's Segmented, ToggleTile and the rest), never
    // the filled violet that joined button pairs and the settings' tabs used to wear. Only the one filled button, in the kit, has it.
    let mut files = Vec::new();
    slint_files(&crates(), &mut files);
    let mut problems = Vec::new();
    for file in files {
        let rel = file.strip_prefix(crates()).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap();
        if (text.contains("Theme.fill") || text.contains("Theme.surface-selected")) && !FILLED.contains(&rel.as_str()) {
            problems.push(format!("{rel} fills something with Theme.fill: mark a picked option with the kit's outline instead"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
