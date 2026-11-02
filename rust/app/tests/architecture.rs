//! Crates only meet through core: no crate but the binary depends on another Phase 1 crate (the Rust side of test_architecture.py).

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::collections::HashMap;
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

/// Where a file sits in its crate, as the path other files reach it by: `crate`, then its folders, then its name.
fn module_of(file: &std::path::Path) -> Vec<String> {
    let text = file.to_string_lossy().replace('\\', "/");
    let inside = text.rsplit_once("/src/").map_or(text.as_str(), |(_, rest)| rest).trim_end_matches(".rs");
    let mut path = vec!["crate".to_string()];
    path.extend(inside.split('/').filter(|part| !["lib", "main", "mod"].contains(part)).map(str::to_string));
    path
}

fn is_punct(token: &TokenTree, wanted: char) -> bool {
    matches!(token, TokenTree::Punct(p) if p.as_char() == wanted)
}

/// Reads the paths in Rust source from its tokens, so comments and strings don't count and macro bodies do.
#[derive(Default)]
struct Paths {
    found: Vec<Vec<String>>,
    /// Names `use` brought in, and where they lead
    names: HashMap<String, Vec<String>>,
}

impl Paths {
    /// Turns what's written into a path from the crate root (`crate::...`) or an outside crate's name.
    fn resolve(&self, written: &[String], module: &[String]) -> Vec<String> {
        let mut base = module.to_vec();
        let mut rest = written;
        match written.first().map(String::as_str) {
            Some("crate") => {
                base.truncate(1);
                rest = &written[1..];
            }
            Some("self") => rest = &written[1..],
            Some("super") => {}
            Some(first) => match self.names.get(first) {
                Some(target) => {
                    base = target.clone();
                    rest = &written[1..];
                }
                None => base = Vec::new(),
            },
            None => {}
        }
        while rest.first().is_some_and(|part| part == "super") {
            base.pop();
            rest = &rest[1..];
        }
        base.extend(rest.iter().filter(|part| *part != "self").cloned());
        base
    }

    /// A path counts when it starts somewhere a file can reach: other bare names are locals, types and the standard library.
    fn note(&mut self, written: &[String], module: &[String]) {
        let first = written[0].as_str();
        if ["crate", "self", "super"].contains(&first) || first.starts_with("telescope_") || self.names.contains_key(first) {
            let path = self.resolve(written, module);
            self.found.push(path);
        }
    }

    /// Each branch of a `{...}` group in a `use`.
    fn use_group(&mut self, group: &proc_macro2::Group, prefix: &[String], module: &[String]) {
        let inner: Vec<TokenTree> = group.stream().into_iter().collect();
        for branch in inner.split(|t| is_punct(t, ',')) {
            self.use_tree(branch, prefix, module);
        }
    }

    /// One branch of a `use`: `a::b`, `a::b as c`, `a::*` or `a::{...}`.
    fn use_tree(&mut self, tokens: &[TokenTree], prefix: &[String], module: &[String]) {
        let mut written = prefix.to_vec();
        let mut named = None;
        for (at, token) in tokens.iter().enumerate() {
            match token {
                TokenTree::Punct(p) if p.as_char() == '*' => written.push("*".to_string()),
                TokenTree::Ident(word) if word == "as" => {
                    named = tokens.get(at + 1).map(|t| t.to_string());
                    break;
                }
                TokenTree::Ident(word) => written.push(word.to_string()),
                TokenTree::Group(group) => return self.use_group(group, &written, module),
                _ => {}
            }
        }
        if written.is_empty() {
            return;
        }
        let path = self.resolve(&written, module);
        let name = named.or_else(|| written.iter().rev().find(|part| *part != "self").cloned()).unwrap_or_default();
        if name != "*" && name != "_" {
            self.names.insert(name, path.clone());
        }
        self.found.push(path);
    }

    fn walk(&mut self, stream: TokenStream, module: &mut Vec<String>) {
        let tokens: Vec<TokenTree> = stream.into_iter().collect();
        let joined = |at: usize| tokens.len() > at + 1 && is_punct(&tokens[at], ':') && is_punct(&tokens[at + 1], ':');
        let mut at = 0;
        while at < tokens.len() {
            match &tokens[at] {
                // A test module or item isn't what the rules are about, so it's skipped up to its end
                TokenTree::Punct(p) if p.as_char() == '#' => {
                    let attribute = tokens.get(at + 1).map(|t| t.to_string().replace(' ', "")).unwrap_or_default();
                    at += 1;
                    if attribute == "[cfg(test)]" {
                        at += 1;
                        while at < tokens.len() && !is_punct(&tokens[at], ';') && !matches!(&tokens[at], TokenTree::Group(g) if g.delimiter() == Delimiter::Brace) {
                            at += 1;
                        }
                        at += 1;
                    }
                }
                TokenTree::Group(group) => {
                    self.walk(group.stream(), module);
                    at += 1;
                }
                TokenTree::Ident(word) if word == "mod" => {
                    if let (Some(TokenTree::Ident(name)), Some(TokenTree::Group(body))) = (tokens.get(at + 1), tokens.get(at + 2)) {
                        module.push(name.to_string());
                        self.walk(body.stream(), module);
                        module.pop();
                        at += 3;
                    } else {
                        at += 2;
                    }
                }
                TokenTree::Ident(word) if word == "use" => {
                    let end = tokens[at..].iter().position(|t| is_punct(t, ';')).map_or(tokens.len(), |n| at + n);
                    self.use_tree(&tokens[at + 1..end], &[], module);
                    at = end + 1;
                }
                TokenTree::Ident(word) => {
                    let mut written = vec![word.to_string()];
                    at += 1;
                    while joined(at) {
                        at += 2;
                        match tokens.get(at) {
                            Some(TokenTree::Ident(next)) => {
                                written.push(next.to_string());
                                at += 1;
                            }
                            // A macro's `crate::{a, b}`
                            Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace => {
                                self.use_group(group, &written, module);
                                at += 1;
                                written.clear();
                                break;
                            }
                            _ => break,
                        }
                    }
                    if !written.is_empty() {
                        self.note(&written, module);
                    }
                }
                _ => at += 1,
            }
        }
    }
}

/// Every path a source file names, resolved from where the file sits: `super::`, grouped imports, renames and macro bodies included.
fn paths_of(text: &str, file: &std::path::Path) -> Vec<Vec<String>> {
    let stream: TokenStream = text.parse().unwrap_or_else(|e| panic!("{} doesn't parse: {e}", file.display()));
    let mut paths = Paths::default();
    // A second pass so a `use` further down still counts for what's above it
    for _ in 0..2 {
        paths.found.clear();
        paths.walk(stream.clone(), &mut module_of(file));
    }
    paths.found
}

fn shown(path: &[String]) -> String {
    path.join("::")
}

/// A plugin's uses of other plugins: reaching into one, or a glob over the folder that holds it.
fn uses_of_plugins(text: &str, file: &std::path::Path, others: &[Vec<String>]) -> Vec<String> {
    paths_of(text, file).iter().filter(|path| {
        others.iter().any(|other| path.starts_with(other)) || path.split_last().is_some_and(|(last, folder)| last == "*" && others.iter().any(|other| other.starts_with(folder)))
    }).map(|path| shown(path)).collect()
}

/// A file's uses of the given top modules of the crate, and of the shell crate from outside it.
fn uses_of_modules(text: &str, file: &std::path::Path, forbidden: &[&str]) -> Vec<String> {
    paths_of(text, file).iter().filter(|path| {
        path[0] == "telescope_shell" || (path[0] == "crate" && path.get(1).is_some_and(|name| forbidden.contains(&name.as_str())))
    }).map(|path| shown(path)).collect()
}

#[test]
fn the_path_reader_sees_every_way_of_naming_a_module() {
    let file = std::path::Path::new("crates/x/src/plugins/card.rs");
    let others = vec![module_of(std::path::Path::new("crates/x/src/plugins/other.rs"))];
    for (text, caught) in [
        ("use crate::plugins::other::Thing;", true),
        ("use super::other::Thing;", true),
        ("use super::{other};", true),
        ("use crate::{plugins::other, x};", true),
        ("use crate::plugins::{other as o, x};", true),
        ("use crate::{plugins::{self, other::{Thing}}};", true),
        ("use super::*;", true),
        ("use crate::plugins::*;", true),
        ("use crate::plugins as p; fn f() { p::other::go(); }", true),
        ("fn f() { p::other::go(); } use crate::plugins as p;", true),
        ("fn f() { let _ = super::other::go(); }", true),
        ("fn f() { println!(\"{}\", super::other::NAME); }", true),
        ("fn f() { let _ = self::super::other::go(); }", true),
        ("mod inner { use super::super::other::Thing; }", true),
        ("fn f() { let _: crate::plugins::other::Thing; }", true),
        ("use super::mine::Thing;", false),
        ("mod inner { use super::other::Thing; }", false),
        ("// use super::other::Thing;\nconst A: &str = \"super::other::go\";", false),
        ("#[cfg(test)]\nmod tests { use super::super::other::Thing; }", false),
        ("#[cfg(test)]\nuse super::other::Thing;\nuse super::fine::Thing as Fine;", false),
        ("use telescope_core::plugin::Plugin; fn f() { Plugin::go(); }", false),
    ] {
        assert_eq!(!uses_of_plugins(text, file, &others).is_empty(), caught, "{text}");
    }
    for (text, caught) in [
        ("use crate::{window::Window, x};", true),
        ("use crate::window;", true),
        ("use super::super::window::Window;", true),
        ("use super::super::{sources as s, x};", true),
        ("use telescope_shell::x;", true),
        ("use super::window::Window;", false),
        ("use telescope_core::session::x;", false),
    ] {
        assert_eq!(!uses_of_modules(text, file, &["window", "sources"]).is_empty(), caught, "{text}");
    }
}

// test_architecture.py::test_a_plugin_imports_no_other_plugin
#[test]
fn a_plugin_uses_no_other_plugin() {
    let all = sources();
    let plugins = plugin_files(&all);
    let mut broken = Vec::new();
    for (krate, path, text) in &plugins {
        let others: Vec<Vec<String>> = plugins.iter().filter(|(k, p, _)| k == krate && p != path).map(|(_, p, _)| module_of(p)).collect();
        for used in uses_of_plugins(text, path, &others) {
            broken.push(format!("{krate}: {} uses {used}", shown(&module_of(path))));
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
        for used in uses_of_modules(text, path, &["window", "session", "sources", "streams", "stream_flow", "shell"]) {
            broken.push(format!("{krate}: {} uses {used}", shown(&module_of(path))));
        }
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

// test_architecture.py::test_the_pieces_the_window_drives_dont_import_it
#[test]
fn the_pieces_the_window_drives_dont_use_it() {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates");
    // plugin.py's twin is the Plugin trait in core
    for name in ["shell/src/sources.rs", "shell/src/session.rs", "shell/src/stream_flow.rs", "core/src/plugin.rs"] {
        let file = crates.join(name);
        let text = std::fs::read_to_string(&file).unwrap();
        let used = uses_of_modules(&text, &file, &["window", "streams"]);
        assert!(used.is_empty(), "{name} uses {}, which drives it", used.join(", "));
    }
}

// test_architecture.py::test_sessions_needs_no_qt
#[test]
fn sessions_needs_no_toolkit() {
    let text = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/shell/src/session.rs")).unwrap();
    assert!(!code(&text).contains("slint"), "session.rs names slint");
}

/// The status kinds a crate's cards colour, read from their rules (`"status_ok" ? Theme.ok`), as Python reads its stylesheet.
fn styled_kinds(slint: &str) -> Vec<String> {
    let mut styled = Vec::new();
    for (at, _) in slint.match_indices("\"status_") {
        let kind: String = slint[at + 1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let tail = slint[at + 2 + kind.len()..].trim_start();
        // A rule can take a short name too: `"status_ok" || kind == "ok" ? Theme.ok`
        let tail = tail.strip_prefix("||").and_then(|t| t.find('?').map(|q| &t[q..])).unwrap_or(tail);
        let Some(colour) = tail.strip_prefix('?').map(str::trim_start) else { continue };
        if !colour.starts_with("Theme.") {
            continue;
        }
        styled.push(kind);
        // The chain's last arm is the colour everything else gets, so a dim one styles status_dim
        let after: String = colour.chars().skip_while(|c| c.is_alphanumeric() || *c == '.' || *c == '-').collect();
        let last = after.trim_start().strip_prefix(':').map_or("", str::trim_start);
        if ["Theme.dim", "Theme.text-dim", "Theme.text-faint"].iter().any(|dim| last.starts_with(dim)) {
            styled.push("status_dim".to_string());
        }
    }
    styled
}

// test_architecture.py::test_status_kinds_are_ones_the_theme_styles
#[test]
fn status_kinds_are_ones_the_cards_style() {
    // A kind the cards don't colour leaves its text unstyled, as "warn" did for Notes, Desk, Chroma key and QR reader
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
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let (mut used, mut unknown, mut rules) = (0, Vec::new(), 0);
    for entry in std::fs::read_dir(&crates).unwrap().flatten() {
        let mut files = Vec::new();
        slint_files(&entry.path(), &mut files);
        // Each crate's cards have their own rules, so a kind has to be styled where it's used
        let styled: Vec<String> = files.iter().flat_map(|file| styled_kinds(&std::fs::read_to_string(file).unwrap())).collect();
        rules += styled.len();
        rust_files(&entry.path(), &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for (at, _) in text.match_indices("\"status_") {
                let kind: String = text[at + 1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                // A kind built up from a word ("status_" + kind) is checked where the word is
                if kind == "status_" {
                    continue;
                }
                // The Microphone card's dialog text is meant to be in the plain text colour, which is what no rule gives
                if kind == "status_none" && file.ends_with("audio/ui/microphone.slint") {
                    continue;
                }
                used += 1;
                if !styled.contains(&kind) {
                    unknown.push(format!("{}: {kind}", file.strip_prefix(&crates).unwrap_or(&file).display()));
                }
            }
        }
    }
    assert!(used > 50, "found only {used} kinds");
    assert!(rules > 20, "found only {rules} colour rules");
    assert!(unknown.is_empty(), "{}", unknown.join("\n"));
}
