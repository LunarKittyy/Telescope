//! A language's catalog (telescope.i18n's _read_catalog and _pseudo_catalog) and where the catalogs come from.

use super::lang::PSEUDO_LANGUAGE;
use super::po::{self, fields, plural_rule, read_po, Plural};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One language's translations, with its plural rule.
#[derive(Debug)]
pub struct Catalog {
    /// In the order of the file: (key, forms).
    entries: Vec<(String, Vec<String>)>,
    index: HashMap<String, usize>,
    plural: Plural,
    /// Translated text to its English, the first English where two texts are translated alike.
    english: HashMap<String, String>,
}

/// Whether a form can be filled with its English's fields, found out as the catalog loads; a spec only counts on a plural's {n}, as other fields go through str::replace.
fn fits(english: &str, plural: &str, form: &str) -> bool {
    let is_plural = !plural.is_empty();
    if super::spec_fields(form).iter().any(|name| !is_plural || name != "n") {
        return false;
    }
    let mut allowed: Vec<(String, u8)> = fields(english).into_iter().chain(fields(plural)).map(|f| (f, 0)).collect();
    allowed.push(("n".into(), 0));
    let values: Vec<(&str, &dyn std::fmt::Display)> = allowed.iter().map(|(k, v)| (k.as_str(), v as &dyn std::fmt::Display)).collect();
    super::fill(form, &values).is_ok()
}

impl Catalog {
    fn build(entries: Vec<(String, Vec<String>)>, plural: Plural) -> Catalog {
        // A later entry for the same text replaces the earlier, in its place (a dict)
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut list: Vec<(String, Vec<String>)> = Vec::new();
        for (key, forms) in entries {
            match index.get(&key) {
                Some(&i) => list[i].1 = forms,
                None => {
                    index.insert(key.clone(), list.len());
                    list.push((key, forms));
                }
            }
        }
        let mut english: HashMap<String, String> = HashMap::new();
        for (key, forms) in &list {
            let (ctx, msgid) = key.split_once('\u{4}').unwrap_or(("", key));
            if ctx != "search" && forms.len() == 1 {
                english.entry(forms[0].clone()).or_insert_with(|| msgid.to_string());
            }
        }
        Catalog { entries: list, index, plural, english }
    }

    /// A .po file as a catalog, without the fuzzy and empty translations and the ones whose fields don't fit their English.
    pub fn from_po(bytes: &[u8]) -> Result<Catalog, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "it isn't UTF-8 text".to_string())?;
        let (entries, header) = read_po(text)?;
        let plural = plural_rule(&header);
        let list = entries.into_iter().filter(|e| !e.fuzzy()).filter_map(|e| {
            let forms: Vec<String> = e.msgstr.iter().map(|form| {
                if form.is_empty() || fits(&e.msgid, &e.plural, form) {
                    return form.clone();
                }
                log::warn!("Translation of {:?} has bad fields, using English", e.msgid);
                String::new()
            }).collect();
            forms.iter().any(|f| !f.is_empty()).then(|| (e.key(), forms))
        }).collect();
        Ok(Catalog::build(list, plural))
    }

    /// The pseudo language: every text of the template longer and accented, but the settings search's words (they're for typing).
    pub fn pseudo_from(template: &[u8]) -> Result<Catalog, String> {
        let text = std::str::from_utf8(template).map_err(|_| "it isn't UTF-8 text".to_string())?;
        let (entries, _header) = read_po(text)?;
        let list = entries.into_iter().filter(|e| e.ctx != "search").map(|e| {
            let mut forms = vec![pseudo(&e.msgid)];
            if !e.plural.is_empty() {
                forms.push(pseudo(&e.plural));
            }
            (e.key(), forms)
        }).collect();
        Ok(Catalog::build(list, Plural::english()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The forms of a text by its key ("English", or "ctx\u{4}English").
    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.index.get(key).map(|&i| self.entries[i].1.as_slice())
    }

    /// Every translated text and its forms, in the order of the file.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// Which plural form fits n, None when the rule can't say (it divides by zero).
    pub fn form(&self, n: i64) -> Option<usize> {
        self.plural.eval(n).and_then(|i| usize::try_from(i).ok())
    }

    /// The English a translated text came from.
    pub fn english(&self, shown: &str) -> Option<&str> {
        self.english.get(shown).map(String::as_str)
    }
}

const ACCENTED_FROM: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const ACCENTED_TO: &str = "àƀçđéƒĝĥîĵķĺɱñöþǫŕšţûṽŵẋýžÀßÇĐÉƑĜĤÎĴĶĹṀÑÖÞǪŔŠŢÛṼŴẊÝŽ";

/// How much longer than the English a pseudo text is, about what German or Finnish need.
pub const PSEUDO_GROWTH: f64 = 0.4;

fn accent(text: &str) -> String {
    text.chars().map(|c| ACCENTED_FROM.chars().position(|f| f == c).and_then(|i| ACCENTED_TO.chars().nth(i)).unwrap_or(c)).collect()
}

/// Python's round(): halves go to the even number.
fn round_half_even(x: f64) -> i64 {
    let floor = x.floor();
    let diff = x - floor;
    if diff > 0.5 || (diff == 0.5 && (floor as i64) % 2 != 0) { floor as i64 + 1 } else { floor as i64 }
}

/// What pseudo() keeps as it is: {fields}, HTML tags and entities.
fn keep() -> &'static regex::Regex {
    static KEEP: OnceLock<regex::Regex> = OnceLock::new();
    KEEP.get_or_init(|| regex::Regex::new(r"\{[^{}]*\}|<[^<>]*>|&[a-zA-Z]+;|&#[0-9]+;").expect("a fixed pattern"))
}

/// The text the way the pseudo language shows it: "Start streaming" is "[Šţàŕţ šţŕéàɱîñĝ ~~~~ ~~]". Its own line breaks stay, so a
/// hand-broken text keeps its lines, each 40% longer.
pub fn pseudo(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut body = String::new();
        let (mut end, mut letters) = (0usize, 0usize);
        for m in keep().find_iter(line) {
            let plain = &line[end..m.start()];
            body += &accent(plain);
            body += m.as_str();
            letters += plain.chars().count();
            end = m.end();
        }
        body += &accent(&line[end..]);
        letters += line[end..].chars().count();
        let extra = round_half_even(letters as f64 * PSEUDO_GROWTH).max(1) as usize;
        let mut tildes: Vec<String> = vec!["~~~~".to_string(); extra / 4];
        tildes.push("~".repeat(extra % 4));
        lines.push(format!("[{body} {}]", tildes.join(" ").trim()));
    }
    lines.join("\n")
}

/// Where the catalogs come from: the files in the program (a shipped build), or a folder of .po files.
#[derive(Clone)]
pub enum Source {
    /// The template and the catalogs by language code, as bytes.
    Embedded { template: &'static [u8], catalogs: &'static [(&'static str, &'static [u8])] },
    Folder(PathBuf),
}

impl Source {
    /// Languages with a catalog, besides English, sorted.
    pub fn available(&self) -> Vec<String> {
        let mut out: Vec<String> = match self {
            Source::Embedded { catalogs, .. } => catalogs.iter().map(|(code, _)| code.to_string()).collect(),
            Source::Folder(dir) => std::fs::read_dir(dir).map(|rd| rd.flatten().filter_map(|e| po_stem(&e.path())).collect()).unwrap_or_default(),
        };
        out.sort();
        out
    }

    fn read(&self, file: &str) -> Result<std::borrow::Cow<'static, [u8]>, String> {
        match self {
            Source::Embedded { template, catalogs } => {
                let found = if file == "telescope.pot" { Some(*template) } else { catalogs.iter().find(|(code, _)| format!("{code}.po") == file).map(|(_, b)| *b) };
                found.map(std::borrow::Cow::Borrowed).ok_or_else(|| format!("no {file}"))
            }
            Source::Folder(dir) => std::fs::read(dir.join(file)).map(std::borrow::Cow::Owned).map_err(|e| format!("{file}: {e}")),
        }
    }

    /// The catalog of a language, or the pseudo language.
    pub fn catalog(&self, lang: &str) -> Result<Catalog, String> {
        if lang == PSEUDO_LANGUAGE {
            return Catalog::pseudo_from(&self.read("telescope.pot")?);
        }
        Catalog::from_po(&self.read(&format!("{lang}.po"))?)
    }

    /// A first draft nobody has checked yet (its header says X-Telescope-Draft: yes): Settings marks it as one.
    pub fn is_draft(&self, lang: &str) -> bool {
        if lang == super::lang::SOURCE_LANGUAGE {
            return false;
        }
        let Ok(bytes) = self.read(&format!("{lang}.po")) else { return false };
        // The header is the first entry, so only that much has to be read
        let Ok(text) = std::str::from_utf8(&bytes) else { return false };
        let head = text.split("\n\n").next().unwrap_or("");
        match read_po(head) {
            Ok((_, header)) => po::header_field(&header, "X-Telescope-Draft").eq_ignore_ascii_case("yes"),
            Err(_) => false,
        }
    }
}

fn po_stem(path: &Path) -> Option<String> {
    (path.extension()? == "po").then(|| path.file_stem().map(|s| s.to_string_lossy().into_owned())).flatten()
}
