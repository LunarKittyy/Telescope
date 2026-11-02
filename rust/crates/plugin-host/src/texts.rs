//! A plugin's own catalogs, kept in step with its code (scripts/i18n.py --plugin): `plugin-texts extract`, `new <lang>` and `status`.
//! Texts come from tr(), trn() and N_() calls with literal strings in its .lua files and from @tr() in its .slint files.

use crate::i18n::{read_po, write_po, PoEntry, LOCALE};
use crate::manifest::MANIFEST;
use std::path::{Path, PathBuf};

const DEFAULT_PLURAL: &str = "nplurals=2; plural=(n != 1);";

/// Languages whose plural rule isn't English's (scripts/i18n.py's PLURALS).
const PLURALS: &[(&str, &str)] = &[
    ("fr", "nplurals=2; plural=(n > 1);"),
    ("pt_BR", "nplurals=2; plural=(n > 1);"),
    ("ja", "nplurals=1; plural=0;"),
    ("ko", "nplurals=1; plural=0;"),
    ("zh_CN", "nplurals=1; plural=0;"),
    ("zh_TW", "nplurals=1; plural=0;"),
    ("vi", "nplurals=1; plural=0;"),
    ("th", "nplurals=1; plural=0;"),
    ("id", "nplurals=1; plural=0;"),
    ("ru", "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);"),
    ("uk", "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);"),
    ("pl", "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);"),
    ("cs", "nplurals=3; plural=(n==1) ? 0 : (n>=2 && n<=4) ? 1 : 2;"),
    ("ar", "nplurals=6; plural=(n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5);"),
];

fn plural_rule(lang: &str) -> Option<&'static str> {
    let base = lang.split('_').next().unwrap_or(lang);
    PLURALS.iter().find(|(l, _)| *l == lang).or_else(|| PLURALS.iter().find(|(l, _)| *l == base)).map(|(_, r)| *r)
}

fn header(lang: &str) -> String {
    let rule = if lang.is_empty() { DEFAULT_PLURAL } else { plural_rule(lang).unwrap_or(DEFAULT_PLURAL) };
    format!("Project-Id-Version: Telescope plugin\nLanguage: {lang}\nMIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\n\
             Content-Transfer-Encoding: 8bit\nPlural-Forms: {rule}\n")
}

fn plural_count(header: &str) -> usize {
    header.split("nplurals=").nth(1).and_then(|r| r.split(';').next()).and_then(|n| n.trim().parse().ok()).unwrap_or(2)
}

/// The plugin a command works on: its folder, name and catalogs.
pub struct Target {
    pub folder: PathBuf,
    pub name: String,
}

impl Target {
    /// The plugin in folder; an error when folder isn't one (no plugin.json naming it).
    pub fn new(folder: &Path) -> Result<Target, String> {
        let manifest = std::fs::read_to_string(folder.join(MANIFEST)).map_err(|_| format!("{} has no {MANIFEST}: point at your plugin's folder", folder.display()))?;
        let value: serde_json::Value = serde_json::from_str(&manifest).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let name = value.get("name").and_then(|v| v.as_str()).ok_or(format!("{MANIFEST} has no \"name\""))?.to_string();
        Ok(Target { folder: folder.to_path_buf(), name })
    }

    pub fn template(&self) -> PathBuf {
        self.folder.join(LOCALE).join(format!("{}.pot", self.name))
    }

    /// Every locale/<lang>.po, sorted.
    pub fn catalogs(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.folder.join(LOCALE)) else { return Vec::new() };
        let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "po")).collect();
        found.sort();
        found
    }
}

/// Every text the plugin's code marks for translation, in file order, each with where it's used.
pub fn find_texts(target: &Target) -> Vec<PoEntry> {
    let mut out: Vec<PoEntry> = Vec::new();
    let files = crate::package::files(&target.folder, true).unwrap_or_default();
    for rel in files {
        let ext = rel.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "lua" && ext != "slint" {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(target.folder.join(&rel)) else { continue };
        let found = if ext == "lua" { lua_texts(&source) } else { slint_texts(&source) };
        let rel = rel.to_string_lossy().replace('\\', "/");
        for (line, msgid, plural) in found {
            let place = format!("{}/{rel}:{line}", target.name);
            match out.iter_mut().find(|e| e.msgid == msgid && e.ctx.is_empty()) {
                Some(e) => e.locations.push(place),
                None => out.push(PoEntry { msgid, forms: vec![String::new(); if plural.is_empty() { 1 } else { 2 }], plural, locations: vec![place], ..Default::default() }),
            }
        }
    }
    out
}

/// A Lua string literal starting at chars[i] ('...' or "..."), and where it ends.
fn lua_string(chars: &[char], i: usize) -> Option<(String, usize)> {
    let quote = *chars.get(i)?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut out = String::new();
    let mut j = i + 1;
    while j < chars.len() {
        match chars[j] {
            c if c == quote => return Some((out, j + 1)),
            '\\' => {
                j += 1;
                out.push(match chars.get(j)? {
                    'n' => '\n',
                    't' => '\t',
                    c => *c,
                });
            }
            '\n' => return None,
            c => out.push(c),
        }
        j += 1;
    }
    None
}

fn skip_space(chars: &[char], mut i: usize) -> usize {
    while chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// (line, msgid, plural) for each tr("..."), N_("...") and trn("...", "...") with literal strings, skipping comments and other strings.
fn lua_texts(source: &str) -> Vec<(usize, String, String)> {
    let chars: Vec<char> = source.chars().collect();
    let line_at = |i: usize| chars[..i].iter().filter(|c| **c == '\n').count() + 1;
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            let long = chars.get(i + 2) == Some(&'[') && chars.get(i + 3) == Some(&'[');
            let end = if long { "]]" } else { "\n" };
            let rest: String = chars[i..].iter().collect();
            i += rest.find(end).map_or(chars.len() - i, |at| rest[..at].chars().count() + end.len());
            continue;
        }
        if c == '"' || c == '\'' {
            i = lua_string(&chars, i).map_or(i + 1, |(_, end)| end);
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while chars.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let after_dot = start > 0 && matches!(chars[start - 1], '.' | ':');
            if after_dot || !matches!(word.as_str(), "tr" | "trn" | "N_") {
                continue;
            }
            let mut j = skip_space(&chars, i);
            let paren = chars.get(j) == Some(&'(');
            if paren {
                j = skip_space(&chars, j + 1);
            }
            let Some((first, end)) = lua_string(&chars, j) else { continue };
            if word == "trn" {
                let k = skip_space(&chars, end);
                if chars.get(k) != Some(&',') {
                    continue;
                }
                let Some((second, end2)) = lua_string(&chars, skip_space(&chars, k + 1)) else { continue };
                out.push((line_at(start), first, second));
                i = end2;
            } else {
                out.push((line_at(start), first, String::new()));
                i = end;
            }
            continue;
        }
        i += 1;
    }
    out
}

/// (line, msgid, "") for each @tr("...") in a .slint file.
fn slint_texts(source: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    for (n, line) in source.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("@tr(") {
            rest = &rest[at + 4..];
            let chars: Vec<char> = rest.trim_start().chars().collect();
            if let Some((text, _)) = lua_string(&chars, 0).filter(|_| chars.first() == Some(&'"')) {
                out.push((n + 1, text, String::new()));
            }
        }
    }
    out
}

/// Brings one language up to date: new texts come in empty, gone ones are kept at the end as #~ for reuse.
fn merge(po: &Path, template: &[PoEntry]) -> Result<(), String> {
    let text = std::fs::read_to_string(po).map_err(|e| format!("{}: {e}", po.display()))?;
    let old = read_po(&text)?;
    let head = old.iter().find(|e| e.msgid.is_empty() && e.ctx.is_empty()).and_then(|e| e.forms.first().cloned()).unwrap_or_else(|| header(""));
    let comments: Vec<String> = text.lines().take_while(|l| l.starts_with("# ")).map(|l| l[2..].to_string()).collect();
    let count = plural_count(&head);
    let mut prior: Vec<PoEntry> = old.into_iter().filter(|e| !e.msgid.is_empty()).collect();
    let mut merged = Vec::new();
    for t in template {
        let mut e = PoEntry { obsolete: false, fuzzy: false, ..t.clone() };
        if let Some(at) = prior.iter().position(|p| p.msgid == t.msgid && p.ctx == t.ctx) {
            let p = prior.remove(at);
            e.forms = p.forms;
            e.fuzzy = p.fuzzy;
        }
        if !e.plural.is_empty() {
            e.forms.resize(count, String::new());
        }
        merged.push(e);
    }
    for mut gone in prior.into_iter().filter(|p| p.forms.iter().any(|f| !f.is_empty())) {
        gone.obsolete = true;
        gone.locations.clear();
        merged.push(gone);
    }
    std::fs::write(po, write_po(&comments, &head, &merged)).map_err(|e| e.to_string())
}

/// Writes locale/<plugin>.pot from the code and brings every locale/<lang>.po up to date with it.
pub fn extract(target: &Target) -> Result<String, String> {
    let entries = find_texts(target);
    std::fs::create_dir_all(target.folder.join(LOCALE)).map_err(|e| e.to_string())?;
    let comments = vec![format!("Texts of the {} plugin. See docs/writing-plugins.md.", target.name)];
    std::fs::write(target.template(), write_po(&comments, &header(""), &entries)).map_err(|e| e.to_string())?;
    let catalogs = target.catalogs();
    for po in &catalogs {
        merge(po, &entries)?;
    }
    Ok(format!("{} texts in {LOCALE}/{}.pot; updated {} languages", entries.len(), target.name, catalogs.len()))
}

/// Starts locale/<lang>.po from the template.
pub fn new(target: &Target, lang: &str) -> Result<String, String> {
    let po = target.folder.join(LOCALE).join(format!("{lang}.po"));
    if po.exists() {
        return Err(format!("{LOCALE}/{lang}.po already exists"));
    }
    let template = std::fs::read_to_string(target.template()).map_err(|_| format!("No {LOCALE}/{}.pot yet: run extract first", target.name))?;
    let entries: Vec<PoEntry> = read_po(&template)?.into_iter().filter(|e| !e.msgid.is_empty()).collect();
    let comments = vec![format!("Texts of the {} plugin in {lang}.", target.name)];
    std::fs::write(&po, write_po(&comments, &header(lang), &[])).map_err(|e| e.to_string())?;
    merge(&po, &entries)?;
    let mut said = format!("Wrote {LOCALE}/{lang}.po");
    if plural_rule(lang).is_none() && !["en", "de", "sv", "nl", "es", "it", "da", "nb", "fi"].contains(&lang.split('_').next().unwrap_or(lang)) {
        said = format!("Check the Plural-Forms line in {lang}.po: it has English's rule\n{said}");
    }
    Ok(said)
}

/// How far each language is: "sv: 1/5 translated".
pub fn status(target: &Target) -> Result<String, String> {
    let mut lines = Vec::new();
    for po in target.catalogs() {
        let text = std::fs::read_to_string(&po).map_err(|e| e.to_string())?;
        let entries: Vec<PoEntry> = read_po(&text)?.into_iter().filter(|e| !e.msgid.is_empty() && !e.obsolete).collect();
        let done = entries.iter().filter(|e| !e.fuzzy && !e.forms.is_empty() && e.forms.iter().all(|f| !f.is_empty())).count();
        let fuzzy = entries.iter().filter(|e| e.fuzzy).count();
        let stem = po.file_stem().unwrap_or_default().to_string_lossy();
        let mut line = format!("{stem}: {done}/{} translated", entries.len());
        if fuzzy > 0 {
            line += &format!(", {fuzzy} to check (fuzzy)");
        }
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_texts_are_found_in_calls_with_literals_only() {
        let src = "-- tr('in a comment')\nlocal s = \"tr('in a string')\"\nlocal a = tr(\"Hello\")\nlocal b = trn('{n} cat', \"{n} cats\", 2)\n\
                   local c = N_ 'Later'\nlocal d = tr(LATER)\nlocal e = obj.tr('method')\n--[[ tr('long comment') ]]\nlocal f = tr('Hi {who}', { who = 1 })\n";
        let all = lua_texts(src);
        let found: Vec<(usize, &str, &str)> = all.iter().map(|(l, a, b)| (*l, a.as_str(), b.as_str())).collect();
        assert_eq!(found, [(3, "Hello", ""), (4, "{n} cat", "{n} cats"), (5, "Later", ""), (9, "Hi {who}", "")]);
        assert_eq!(slint_texts("label: @tr(\"Strength\");\n"), [(1, "Strength".to_string(), String::new())]);
    }
}
