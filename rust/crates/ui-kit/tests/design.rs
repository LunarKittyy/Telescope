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
    ("effects/ui/composite_editor.slint", 7, 3),
    ("effects/ui/parts.slint", 1, 1),
    ("setup/ui/parts.slint", 0, 1),
    ("shell/ui/cards/onboarding.slint", 1, 0),
    ("shell/ui/cards/preview.slint", 1, 1),
    ("shell/ui/frame.slint", 1, 1),
    ("shell/ui/preview.slint", 2, 1),
    ("shell/ui/settings.slint", 1, 5),
    ("system/ui/mcp.slint", 1, 1),
    ("system/ui/shortcuts.slint", 1, 2),
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

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if name != "target" && name != ".git" && name != "node_modules" {
                source_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| ["slint", "rs", "py", "ps1", "toml"].iter().any(|k| x == *k)) {
            out.push(p);
        }
    }
}

#[test]
fn every_icon_file_is_used_and_no_two_are_the_same_drawing() {
    let icons = kit().join("ui/icons");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&icons).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "svg")).collect();
    files.sort();
    assert!(files.len() > 50, "{} icons", files.len());
    let mut sources = Vec::new();
    source_files(&crates().join(".."), &mut sources);
    let texts: Vec<String> = sources.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
    let mut orphans = Vec::new();
    let mut same: BTreeMap<Vec<u8>, Vec<String>> = BTreeMap::new();
    for file in &files {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        // An icon is used by its path (icons/name.svg in icons.slint) or by its name as a string (a plugin's icon, "v_stream")
        let (path, quoted) = (format!("icons/{stem}.svg"), format!("\"{stem}\""));
        if !texts.iter().any(|t| t.contains(&path) || t.contains(&quoted)) {
            orphans.push(stem.clone());
        }
        same.entry(std::fs::read(file).unwrap()).or_default().push(stem);
    }
    assert!(orphans.is_empty(), "icons nothing refers to, delete them or use them: {orphans:?}");
    let twins: Vec<_> = same.values().filter(|names| names.len() > 1).collect();
    assert!(twins.is_empty(), "byte-identical icons, keep one file per drawing: {twins:?}");
    // And icons.slint only names files that exist
    let table = std::fs::read_to_string(kit().join("ui/icons.slint")).unwrap();
    for part in table.split("icons/").skip(1) {
        let name = part.split('"').next().unwrap();
        assert!(icons.join(name).is_file(), "icons.slint names icons/{name}, which isn't there");
    }
}

/// Every .slint file of the kit and of the cards, with its path.
fn all_slint() -> Vec<(String, String)> {
    let mut files = Vec::new();
    slint_files(&crates(), &mut files);
    let mut kit_files = Vec::new();
    slint_files(&kit().join("ui"), &mut kit_files);
    files.extend(kit_files);
    files.sort();
    files.dedup();
    files.into_iter().map(|f| (f.display().to_string(), std::fs::read_to_string(&f).unwrap())).collect()
}

/// A motion is a spring that keeps its place and speed when it is given a new target (see STYLE.md, Motion). Nothing may build one that
/// doesn't chain: no `delay` (a retarget freezes the value for the delay, then it jumps on with its old speed), no curve on a number that
/// moves (`cubic-bezier`, `ease-*` restart from rest at every retarget), and no `snap` that a key sets (a key during a move cut it dead).
#[test]
fn no_animation_has_a_delay_a_restarting_curve_or_a_snap_from_a_key() {
    let mut problems = Vec::new();
    for (path, text) in all_slint() {
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap();
            let at = || format!("{path}:{}: {}", n + 1, line.trim());
            if code.contains("delay:") && !code.contains("property") {
                problems.push(format!("a delay: {}", at()));
            }
            if code.contains("cubic-bezier") || code.contains("easing: ease") || code.contains("easing: linear") {
                problems.push(format!("a curve that restarts at every retarget (use a spring): {}", at()));
            }
            if code.contains("snap:") && code.contains("quick") {
                // `quick` is what a key sets; the colours may follow it at once (Tone), places and sizes may not
                if !code.contains("Tone {") {
                    problems.push(format!("a snap that a key sets: {}", at()));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// A stroked Path is fitted by Slint into its box less one stroke, offset by half of it, so a path whose viewbox is as big as its box is
/// drawn smaller than its numbers and off the pixel grid. In the kit, every Path that has a stroke and a viewbox says which way it is
/// fitted: its viewbox is the box less a stroke (written with `stroke`), or it widens its box by the stroke, or it says `fit: fill`.
#[test]
fn a_stroked_path_in_the_kit_is_fitted_on_purpose() {
    let mut problems = Vec::new();
    for (path, text) in all_slint() {
        if !path.contains("/ui-kit/ui/") {
            continue;
        }
        for (n, block) in text.split("Path {").enumerate().skip(1) {
            let body = block.split("\n    }\n").next().unwrap_or(block);
            let body = body.split("\n}\n").next().unwrap_or(body);
            if body.contains("stroke-width:") && body.contains("viewbox-width:") {
                let on_purpose = body.contains("Theme.stroke /") || body.contains("- Theme.stroke") || body.contains("ImageFit.fill") || body.contains("fitted:");
                if !on_purpose {
                    problems.push(format!("{path}: Path number {n} has a stroke and a viewbox of its own size; subtract the stroke from the viewbox (see widgets/stepper.slint) or say `fit: ImageFit.fill`, or write a `fitted:` comment in the Path saying why any scale will do"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The words a fold's title says about its state ("Expanded. Press Space..." / "Collapsed...") are for a screen reader only: they may be read
/// into an accessible-description, never drawn and never a tooltip's name or body.
#[test]
fn a_folds_state_words_are_for_screen_readers_only() {
    let mut problems = Vec::new();
    for (path, text) in all_slint() {
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap();
            if (code.contains("FoldText.expanded") || code.contains("FoldText.collapsed")) && !code.contains("accessible-description") {
                problems.push(format!("{path}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(problems.is_empty(), "FoldText belongs in accessible-description only:\n{}", problems.join("\n"));
}
