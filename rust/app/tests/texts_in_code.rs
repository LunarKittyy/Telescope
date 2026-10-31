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

/// The argument of a call opening just before `s`, up to its closing parenthesis: how long it is, strings stepped over.
fn call_end(s: &str) -> Option<usize> {
    let mut depth = 1;
    let mut at = 0;
    while at < s.len() {
        let rest = &s[at..];
        if rest.starts_with('"') {
            at += literal(rest)?.1;
            continue;
        }
        match rest.chars().next()? {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
        at += rest.chars().next()?.len_utf8();
    }
    None
}

/// The {fields} a text fills in (i18n.FIELD): {{ and }} are braces, not fields.
fn fields(text: &str) -> BTreeSet<String> {
    let text = text.replace("{{", "").replace("}}", "");
    text.split('{').skip(1).filter_map(|t| t.split_once('}')).map(|(f, _)| f.split([':', '!']).next().unwrap_or("").to_string())
        .filter(|f| f.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && f.chars().all(|c| c.is_alphanumeric() || c == '_')).collect()
}

/// What the .field("name", ...) calls right after a tr(...) fill in; None when there are none, so whatever gets the text fills it in.
fn chained_fields(after: &str) -> Option<BTreeSet<String>> {
    let (mut rest, mut got) = (after, None::<BTreeSet<String>>);
    while let Some(tail) = rest.trim_start().strip_prefix(".field(") {
        got.get_or_insert_default().insert(literal(tail.trim_start())?.0);
        rest = &tail[call_end(tail)? + 1..];
    }
    got
}

// test_i18n.py::test_every_text_can_be_found, what find_texts checks: n_() and trn() take plain literals, the fields filled in are the
// text's own, and one text has one plural
#[test]
fn every_text_can_be_found() {
    let mut files = Vec::new();
    sources(&root().join("crates"), &mut files);
    sources(&root().join("app/src"), &mut files);
    let mut bad = Vec::new();
    let mut plurals: std::collections::BTreeMap<String, (String, String)> = Default::default();
    // The plugin host hands on what a plugin passes, which is no text of the app's
    for file in files.iter().filter(|f| f.extension().is_some_and(|e| e == "rs") && !f.components().any(|c| c.as_os_str() == "plugin-host")) {
        let text = std::fs::read_to_string(file).unwrap();
        let place = |at: usize| format!("{}:{}", file.strip_prefix(root()).unwrap_or(file).display(), text[..at].matches('\n').count() + 1);
        for name in ["tr(", "n_(", "trn("] {
            for (at, _) in text.match_indices(name) {
                let line = text[..at].rsplit('\n').next().unwrap_or("");
                if line.contains("//") || text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') || text[..at].trim_end().ends_with("fn") {
                    continue;
                }
                let inside = &text[at + name.len()..];
                let Some(end) = call_end(inside) else { continue };
                let first = inside.trim_start();
                let Some((msgid, len)) = literal(first) else {
                    if name != "tr(" {
                        bad.push(format!("{}: {name}) needs plain string literals", place(at)));
                    }
                    continue;
                };
                let mut wanted = fields(&msgid);
                let mut given = chained_fields(&inside[end + 1..]);
                if name == "trn(" {
                    let tail = first[len..].trim_start().strip_prefix(',').unwrap_or("").trim_start();
                    let Some((plural, _)) = literal(tail) else {
                        bad.push(format!("{}: trn) needs plain string literals", place(at)));
                        continue;
                    };
                    wanted.extend(fields(&plural));
                    wanted.remove("n");
                    if let Some(g) = given.as_mut() {
                        g.remove("n");
                    }
                    let here = plurals.entry(msgid.clone()).or_insert((plural.clone(), place(at)));
                    if here.0 != plural {
                        bad.push(format!("{}: {msgid:?} has another plural at {}", place(at), here.1));
                    }
                }
                if let Some(given) = given.filter(|g| *g != wanted) {
                    bad.push(format!("{}: {name}) fills in {given:?} but the text has {wanted:?}", place(at)));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} texts can't be found or filled in:\n{}", bad.len(), bad.join("\n"));
}

/// What the ledger says of each text of the template, by its key there.
fn ledger_texts() -> std::collections::BTreeMap<String, String> {
    let ledger = std::fs::read_to_string(root().join("parity/ledger.toml")).unwrap();
    let section = ledger.split("\n[text]\n").nth(1).unwrap().split("\n[").next().unwrap();
    section.lines().filter_map(|line| {
        let (key, len) = literal(line)?;
        let (status, _) = literal(line[len..].split("status = ").nth(1)?)?;
        Some((key, status))
    }).collect()
}

// test_i18n.py::test_the_template_has_every_text_in_the_code, the other way round: until the rewrite has every text, what the template
// holds and the code doesn't has to be one the ledger doesn't call done; and a plural is the template's plural
#[test]
fn the_template_has_nothing_the_ledger_calls_done_that_the_code_lacks() {
    let template = std::fs::read_to_string(root().join("../desktop/telescope/locale/telescope.pot")).unwrap();
    let (entries, _) = po::read_po(&template).unwrap();
    let mut files = Vec::new();
    sources(&root().join("crates"), &mut files);
    sources(&root().join("app/src"), &mut files);
    let mut found: BTreeSet<(String, String)> = BTreeSet::new();
    let mut wrong = Vec::new();
    // Handed to tr() through a helper of the plugin's (with_version, a table): the literal is what counts
    let mut literals: BTreeSet<String> = BTreeSet::new();
    // i18n.rs is left out of the scan above, and says these two itself
    for file in files.iter().chain([&root().join("crates/core/src/i18n.rs")]) {
        let text = std::fs::read_to_string(file).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find('"') {
            rest = &rest[at..];
            match literal(rest) {
                Some((l, len)) => {
                    literals.insert(l);
                    rest = &rest[len..];
                }
                None => rest = &rest[1..],
            }
        }
        found.extend(texts_in(file, &text).into_iter().map(|(_, ctx, english)| (ctx, english)));
        for (at, _) in text.match_indices("trn(") {
            let rest = text[at + 4..].trim_start();
            let Some((singular, len)) = literal(rest) else { continue };
            let Some((plural, _)) = literal(rest[len..].trim_start().strip_prefix(',').unwrap_or("").trim_start()) else { continue };
            if entries.iter().any(|e| e.msgid == singular && e.plural != plural) {
                wrong.push(format!("{singular:?} with the plural {plural:?}"));
            }
            found.insert((String::new(), plural));
        }
    }
    let ledger = ledger_texts();
    let missing: Vec<String> = entries.iter().filter(|e| !found.contains(&(e.ctx.clone(), e.msgid.clone())) && !literals.contains(&e.msgid)).filter_map(|e| {
        let key = if e.ctx.is_empty() { e.msgid.clone() } else { format!("{}|{}", e.ctx, e.msgid) };
        (ledger.get(&key).map(String::as_str) != Some("todo") && ledger.get(&key).map(String::as_str) != Some("dropped")).then_some(key)
    }).collect();
    assert!(wrong.is_empty(), "plurals the template doesn't have:\n{}", wrong.join("\n"));
    assert!(missing.is_empty(), "{} texts marked done that the code doesn't have:\n{}", missing.len(), missing.join("\n"));
}
