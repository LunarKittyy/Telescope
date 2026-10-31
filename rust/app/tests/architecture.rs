//! Crates only meet through core: no crate but the binary depends on another Phase 1 crate (the Rust side of test_architecture.py).

use std::path::PathBuf;

/// Which telescope crates each crate may use (ui-kit is a leaf every card needs); anything else has to go through a trait in core.
const ALLOWED: &[(&str, &[&str])] = &[
    ("core", &[]),
    ("ui-kit", &[]),
    ("link", &["telescope-core", "telescope-ui-kit"]),
    ("media", &["telescope-core", "telescope-ui-kit"]),
    ("outputs", &["telescope-core", "telescope-ui-kit"]),
    ("audio", &["telescope-core", "telescope-ui-kit"]),
    ("system", &["telescope-core", "telescope-ui-kit"]),
    ("plugin-host", &["telescope-core", "telescope-ui-kit"]),
    ("effects", &["telescope-core", "telescope-ui-kit"]),
    ("setup", &["telescope-core", "telescope-ui-kit"]),
    ("vision", &["telescope-core"]),
    ("shell", &["telescope-core", "telescope-ui-kit"]),
];

/// What a crate's own tests may use on top: real video and real shaders need media's decoder and frame pipeline.
const TESTS_ALSO: &[(&str, &[&str])] = &[("link", &["telescope-media"]), ("effects", &["telescope-media"])];

/// The telescope crates a manifest names, each with whether only its tests use it.
fn telescope_deps(manifest: &str) -> Vec<(String, bool)> {
    let (mut in_deps, mut dev) = (false, false);
    let mut deps = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_deps = line.ends_with("dependencies]");
            dev = line == "[dev-dependencies]";
        } else if in_deps && line.starts_with("telescope-") {
            deps.push((line.split(['.', ' ', '=']).next().unwrap().to_string(), dev));
        }
    }
    deps
}

#[test]
fn crates_only_meet_through_core() {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let mut found: Vec<String> = std::fs::read_dir(&crates).unwrap().flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    found.sort();
    for name in &found {
        let allowed = ALLOWED.iter().find(|(n, _)| n == name).map(|(_, a)| *a)
            .unwrap_or_else(|| panic!("crates/{name} isn't in ALLOWED: say which crates it may use"));
        let manifest = std::fs::read_to_string(crates.join(name).join("Cargo.toml")).unwrap();
        let tests_also = TESTS_ALSO.iter().find(|(n, _)| n == name).map_or(&[][..], |(_, a)| *a);
        for (dep, dev) in telescope_deps(&manifest) {
            assert!(allowed.contains(&dep.as_str()) || (dev && tests_also.contains(&dep.as_str())), "crates/{name} depends on {dep}; go through a trait in core instead");
        }
    }
}

// ── Who may use whom inside a crate (the rest of test_architecture.py) ───────────────────────────────────────────────────

fn rust_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !["target", "tests", "fixtures", "example-plugins", "bin"].contains(&name.as_str()) {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
            out.push(path);
        }
    }
}

/// Every non-test source file of every crate but core, as (crate, path, text).
fn sources() -> Vec<(String, PathBuf, String)> {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let mut files = Vec::new();
        rust_files(&entry.path().join("src"), &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            out.push((name.clone(), file, text));
        }
    }
    out
}

/// The text of a file before its test module.
fn code(text: &str) -> &str {
    text.find("\n#[cfg(test)]").map_or(text, |at| &text[..at])
}

/// The files that are a plugin: they implement it. A card's test double or the shell's own list of scenes isn't one.
fn plugin_files(all: &[(String, PathBuf, String)]) -> Vec<&(String, PathBuf, String)> {
    all.iter().filter(|(krate, path, text)| krate != "core" && code(text).contains("impl Plugin for ") && !path.ends_with("shell/src/scenes.rs") && !path.ends_with("shell/src/shell.rs")).collect()
}

fn module_of(path: &std::path::Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// The first names after `crate::` (and after `plugins::`, where the shell keeps its own) in a text.
fn crate_paths(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (at, _) in text.match_indices("crate::") {
        let rest = &text[at + 7..];
        let word: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if word == "plugins" {
            let after = &rest[word.len()..];
            if let Some(inner) = after.strip_prefix("::") {
                names.push(inner.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect());
            }
        }
        names.push(word);
    }
    names
}

// test_architecture.py::test_a_plugin_imports_no_other_plugin
#[test]
fn a_plugin_uses_no_other_plugin() {
    let all = sources();
    let plugins = plugin_files(&all);
    let mut broken = Vec::new();
    for (krate, path, text) in &plugins {
        let me = module_of(path);
        let others: Vec<String> = plugins.iter().filter(|(k, p, _)| k == krate && module_of(p) != me).map(|(_, p, _)| module_of(p)).collect();
        for name in crate_paths(code(text)) {
            if others.contains(&name) {
                broken.push(format!("{krate}: {me} uses {name}"));
            }
        }
    }
    assert!(plugins.len() > 20, "found only {} plugins", plugins.len());
    assert!(broken.is_empty(), "share it through a module that isn't a plugin, the bus or the host:\n{}", broken.join("\n"));
}

// test_architecture.py::test_a_plugin_imports_neither_the_window_nor_the_sessions
#[test]
fn a_plugin_uses_neither_the_window_nor_the_sessions() {
    let all = sources();
    let mut broken = Vec::new();
    for (krate, path, text) in plugin_files(&all) {
        let names = crate_paths(code(text));
        for forbidden in ["window", "session", "sources", "streams", "stream_flow", "shell"] {
            if names.iter().any(|n| n == forbidden) || code(text).contains("telescope_shell") {
                broken.push(format!("{krate}: {} uses {forbidden}", module_of(path)));
            }
        }
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

// test_architecture.py::test_the_pieces_the_window_drives_dont_import_it
#[test]
fn the_pieces_the_window_drives_dont_use_it() {
    let shell = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/shell/src");
    for name in ["sources.rs", "session.rs", "stream_flow.rs"] {
        let text = std::fs::read_to_string(shell.join(name)).unwrap();
        let names = crate_paths(code(&text));
        for window in ["window", "streams"] {
            assert!(!names.iter().any(|n| n == window), "{name} uses {window}, which drives it");
        }
    }
}

// test_architecture.py::test_sessions_needs_no_qt
#[test]
fn sessions_needs_no_toolkit() {
    let text = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/shell/src/session.rs")).unwrap();
    assert!(!code(&text).contains("slint"), "session.rs names slint");
}

// test_architecture.py::test_status_kinds_are_ones_the_theme_styles
#[test]
fn status_kinds_are_ones_the_cards_style() {
    // A kind the cards don't colour leaves its text unstyled, as "warn" did for Notes, Desk, Chroma key and QR reader
    // status_none is the Microphone card's dialog text, in the plain text colour
    const STYLED: [&str; 5] = ["status_ok", "status_warn", "status_err", "status_dim", "status_none"];
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    fn slint_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "target" && n != "example-plugins") {
                    slint_files(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "slint") {
                out.push(path);
            }
        }
    }
    slint_files(&crates, &mut files);
    let (mut used, mut unknown) = (0, Vec::new());
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (at, _) in text.match_indices("\"status_") {
            let kind: String = text[at + 1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            // A kind built up from a word ("status_" + kind) is checked where the word is
            if kind == "status_" {
                continue;
            }
            used += 1;
            if !STYLED.contains(&kind.as_str()) {
                unknown.push(format!("{}: {kind}", file.strip_prefix(&crates).unwrap_or(&file).display()));
            }
        }
    }
    assert!(used > 50, "found only {used} kinds");
    assert!(unknown.is_empty(), "{}", unknown.join("\n"));
}
