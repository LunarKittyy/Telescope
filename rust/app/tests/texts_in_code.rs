//! Every text the Rust code shows through tr(), n_(), trn() and Slint's @tr is a text of the template (desktop/telescope/locale/
//! telescope.pot), so it has the translations the Python app's catalogs hold, and none is spelled a little differently and stays
//! English everywhere. The other way round is the ledger's job: inventory.py lists every text of the template and what ports it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use telescope_core::i18n::po;

/// The files that read or write the calls themselves, so their examples aren't texts.
const READERS: [&str; 4] = ["source.rs", "texts.rs", "text.rs", "i18n.rs"];

/// What the code says that the template doesn't, and why. A text that's new here is a spelling that stays English in every language.
const NOT_IN_THE_TEMPLATE: [(&str, &str); 12] = [
    // Qt's own button texts, which Qt's translations cover in the Python app
    ("OK", "Qt standard button"),
    ("Yes", "Qt standard button"),
    ("No", "Qt standard button"),
    // Texts that only the Rust app has: Slint has no inline links, and its accessibility labels and Lua plugins are new
    ("Pairing code", "an accessibility label"),
    ("Page", "an accessibility label"),
    ("That source has gone.", "only in the Rust app"),
    ("The browser has gone.", "only in the Rust app"),
    ("Get adb from Google", "the Add phone link as a button"),
    ("Telescope can download it from Google (about {size}) and keep it for your user. It's Google's software, so their Android SDK License applies to it: {url}", "the adb question without its link"),
    ("Its {file} has to return the plugin's table at the end.", "a Lua plugin's message"),
    ("Press a key…", "the shortcut editor's hint"),
    ("...", "an example in a comment"),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !["target", "tests", "fixtures", "example-plugins"].contains(&name.as_str()) {
                sources(&path, out);
            }
        } else if (name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" && !READERS.contains(&name.as_str())) || name.ends_with(".slint") {
            out.push(path);
        }
    }
}

/// The string literal at the start of s (Rust or Slint escapes) and its length in bytes.
fn literal(s: &str) -> Option<(String, usize)> {
    let mut chars = s.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, i + 1)),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                // A line continuation takes the next line's indent with it
                '\n' => {
                    while s[chars.offset()..].starts_with([' ', '\t']) {
                        chars.next();
                    }
                }
                'u' => {
                    let rest = &s[chars.offset()..];
                    let end = rest.find('}')?;
                    out.push(char::from_u32(u32::from_str_radix(&rest[1..end], 16).ok()?)?);
                    for _ in 0..=end {
                        chars.next();
                    }
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// The text of a call like `tr("...")` or `@tr("ctx" => "...")` starting after its opening parenthesis: (context, English).
fn after_call(rest: &str, with_context_first: bool) -> Option<(String, String)> {
    let rest = rest.trim_start();
    let (first, len) = literal(rest)?;
    let tail = rest[len..].trim_start();
    if with_context_first {
        let (second, _) = literal(tail.strip_prefix(',').or_else(|| tail.strip_prefix("=>"))?.trim_start())?;
        return Some((first, second));
    }
    if let Some(after) = tail.strip_prefix("=>") {
        return Some((first, literal(after.trim_start())?.0));
    }
    Some((String::new(), first))
}

/// (place, context, English) of every shown text in a source file.
fn texts_in(path: &Path, text: &str) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    let is_slint = path.extension().is_some_and(|e| e == "slint");
    let calls: &[(&str, bool)] = if is_slint { &[("@tr(", false)] } else { &[("tr(", false), ("n_(", false), ("trn(", false), ("tr_ctx(", true), ("tr_with(", false), ("trn_with(", false)] };
    for (name, ctx_first) in calls {
        for (at, _) in text.match_indices(name) {
            let before = text[..at].chars().next_back();
            // Not the end of another name (`str(`, `fn_tr(`), nor a definition
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') || text[..at].trim_end().ends_with("fn") {
                continue;
            }
            let Some((mut ctx, english)) = after_call(&text[at + name.len()..], *ctx_first) else { continue };
            if *name == "tr_with(" {
                // tr_with(msgid, ctx, fields): the context is the next argument
                let rest = text[at + name.len()..].trim_start();
                let after = rest[literal(rest).map_or(0, |l| l.1)..].trim_start().strip_prefix(',').unwrap_or("").trim_start();
                ctx = literal(after).map(|l| l.0).unwrap_or_default();
            }
            let line = text[..at].matches('\n').count() + 1;
            found.push((format!("{}:{line}", path.strip_prefix(root()).unwrap_or(path).display()), ctx, english));
        }
    }
    found
}

#[test]
fn every_text_in_the_code_is_in_the_template() {
    let template = std::fs::read_to_string(root().join("../desktop/telescope/locale/telescope.pot")).unwrap();
    let (entries, _) = po::read_po(&template).unwrap();
    let known: BTreeSet<(String, String)> = entries.iter().map(|e| (e.ctx.clone(), e.msgid.clone())).chain(entries.iter().filter(|e| !e.plural.is_empty()).map(|e| (e.ctx.clone(), e.plural.clone()))).collect();
    let mut files = Vec::new();
    sources(&root().join("crates"), &mut files);
    sources(&root().join("app/src"), &mut files);
    let mut missing: Vec<String> = Vec::new();
    let mut seen = 0;
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (place, ctx, english) in texts_in(&file, &text) {
            seen += 1;
            if !known.contains(&(ctx.clone(), english.clone())) && !NOT_IN_THE_TEMPLATE.iter().any(|(t, _)| *t == english) {
                missing.push(format!("{place}: {ctx:?} {english:?}"));
            }
        }
    }
    assert!(seen > 500, "found only {seen} texts, so the scan is wrong");
    for (text, why) in NOT_IN_THE_TEMPLATE {
        assert!(!known.iter().any(|(_, m)| m == text), "{text:?} ({why}) is in the template now, so it can come off the list");
    }
    assert!(missing.is_empty(), "{} texts that aren't in telescope.pot:\n{}", missing.len(), missing.join("\n"));
}

// test_i18n.py::test_every_text_can_be_found: a text built with format!() can't be found by an extractor, and so can't be translated
#[test]
fn no_text_is_built_from_pieces() {
    let mut files = Vec::new();
    sources(&root().join("crates"), &mut files);
    sources(&root().join("app/src"), &mut files);
    let mut bad = Vec::new();
    for file in files.iter().filter(|f| f.extension().is_some_and(|e| e == "rs")) {
        let text = std::fs::read_to_string(file).unwrap();
        for name in ["tr(", "n_(", "tr_ctx(", "tr_with(", "trn(", "trn_with("] {
            for (at, _) in text.match_indices(name) {
                if text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let rest = text[at + name.len()..].trim_start();
                if rest.starts_with("format!") || rest.starts_with("&format!") {
                    bad.push(format!("{}:{}", file.strip_prefix(root()).unwrap_or(file).display(), text[..at].matches('\n').count() + 1));
                }
            }
        }
    }
    assert!(bad.is_empty(), "texts built with format!() instead of {{fields}}:\n{}", bad.join("\n"));
}
