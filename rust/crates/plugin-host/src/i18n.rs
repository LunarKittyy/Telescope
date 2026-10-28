//! A plugin's own translations: locale/<lang>.po in its folder, used for its texts before Telescope's, then English (add_plugin_catalog).
//! A file that can't be read costs the plugin its translation and nothing else.

use crate::text::fill;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;

/// A plugin's catalogs: <plugin folder>/locale/<lang>.po.
pub const LOCALE: &str = "locale";

/// (context, English) to the translated forms: one for tr(), one per plural form for trn().
type Entries = HashMap<(String, String), Vec<String>>;

/// One language's translations from one .po file, with its plural rule.
#[derive(Debug, Default)]
pub struct Catalog {
    entries: Entries,
    plural: Option<Plural>,
}

impl Catalog {
    /// The plugin's catalog for lang (or the language without its region: pt.po for pt_BR); None in English, without one, or when it's broken.
    pub fn for_plugin(name: &str, folder: &Path, lang: &str) -> Option<Catalog> {
        if lang.is_empty() || lang == "en" {
            return None;
        }
        let base = lang.split('_').next().unwrap_or(lang);
        let tried: &[&str] = if base == lang { &[lang] } else { &[lang, base] };
        for code in tried {
            let path = folder.join(LOCALE).join(format!("{code}.po"));
            if !path.is_file() {
                continue;
            }
            return match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| Catalog::parse(&b)) {
                Ok(c) if !c.entries.is_empty() => Some(c),
                Ok(_) => None,
                Err(e) => {
                    log::warn!("Plugin {name}: couldn't read {code}.po, its texts stay in English: {e}");
                    None
                }
            };
        }
        None
    }

    /// The translations in a .po file; fuzzy and empty ones count as missing.
    pub fn parse(bytes: &[u8]) -> Result<Catalog, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "it isn't UTF-8 text".to_string())?;
        let mut entries = Entries::new();
        let mut header = String::new();
        for entry in read_po(text.trim_start_matches('\u{feff}'))? {
            if entry.msgid.is_empty() && entry.ctx.is_empty() {
                header = entry.forms.first().cloned().unwrap_or_default();
                continue;
            }
            if entry.fuzzy || entry.forms.iter().all(String::is_empty) {
                continue;
            }
            entries.insert((entry.ctx, entry.msgid), entry.forms);
        }
        let plural = header.lines().find_map(|l| l.trim().strip_prefix("Plural-Forms:")).map(|rule| {
            let expr = rule.split(';').find_map(|p| p.trim().strip_prefix("plural=")).unwrap_or("n != 1");
            Plural::parse(expr)
        }).transpose()?;
        Ok(Catalog { entries, plural })
    }

    fn get(&self, ctx: &str, msgid: &str) -> Option<&Vec<String>> {
        self.entries.get(&(ctx.to_string(), msgid.to_string()))
    }

    /// Which plural form fits n, None when the rule can't say (it divides by zero).
    fn form(&self, n: i64) -> Option<usize> {
        match &self.plural {
            Some(p) => p.eval(n).and_then(|i| usize::try_from(i).ok()),
            None => Some((n != 1) as usize),
        }
    }
}

/// tr() for a plugin: its catalog, then Telescope's own (core's tr, which has no contexts), then the English, with {fields} filled in.
pub fn tr(catalog: Option<&Catalog>, msgid: &str, ctx: &str, fields: &[(&str, &dyn Display)]) -> String {
    let english = || fill(msgid, fields).unwrap_or_else(|_| msgid.to_string());
    let translated = catalog.and_then(|c| c.get(ctx, msgid)).and_then(|f| f.first()).filter(|t| !t.is_empty()).cloned()
        .or_else(|| (ctx.is_empty()).then(|| telescope_core::i18n::tr(msgid)).filter(|t| t != msgid));
    match translated {
        Some(t) => fill(&t, fields).unwrap_or_else(|_| {
            log::warn!("Translation of {msgid:?} has bad fields, using English");
            english()
        }),
        None => english(),
    }
}

/// trn() for a plugin: the form of its catalog's translation that fits n, else the English one, with {n} and the fields filled in.
pub fn trn(catalog: Option<&Catalog>, singular: &str, plural: &str, n: i64, ctx: &str, fields: &[(&str, &dyn Display)]) -> String {
    let mut all: Vec<(&str, &dyn Display)> = vec![("n", &n)];
    all.extend_from_slice(fields);
    let english = if n == 1 { singular } else { plural };
    let fallback = || fill(english, &all).unwrap_or_else(|_| english.to_string());
    let found = catalog.and_then(|c| Some((c.get(ctx, singular)?, c.form(n)?))).and_then(|(forms, i)| forms.get(i)).filter(|t| !t.is_empty());
    match found {
        Some(t) => fill(t, &all).unwrap_or_else(|_| fallback()),
        None => fallback(),
    }
}

#[derive(Debug, Default)]
struct Entry {
    ctx: String,
    msgid: String,
    forms: Vec<String>,
    fuzzy: bool,
}

/// The entries of a .po file (i18n.read_po), strictly: a line it doesn't understand makes the file broken.
fn read_po(text: &str) -> Result<Vec<Entry>, String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Field {
        Ctx,
        Id,
        IdPlural,
        Str(usize),
    }
    let mut entries = Vec::new();
    let mut entry = Entry::default();
    let mut started = false;
    let mut field: Option<Field> = None;
    let mut flags_fuzzy = false;
    fn push(entries: &mut Vec<Entry>, entry: &mut Entry, started: &mut bool) {
        if *started {
            entries.push(std::mem::take(entry));
        }
        *started = false;
    }
    for (number, raw) in text.lines().enumerate() {
        let mut line = raw.trim();
        if let Some(rest) = line.strip_prefix("#~") {
            line = rest.trim();
            // An obsolete entry: read for its shape, then dropped
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
        }
        if line.is_empty() {
            push(&mut entries, &mut entry, &mut started);
            field = None;
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if started && field.is_some() {
                push(&mut entries, &mut entry, &mut started);
                field = None;
            }
            if let Some(flags) = comment.strip_prefix(',') {
                flags_fuzzy |= flags.split(',').any(|f| f.trim() == "fuzzy");
            }
            continue;
        }
        let bad = || format!("line {}: {raw:?} isn't .po", number + 1);
        let (keyword, rest) = match line.find(|c: char| c.is_whitespace() || c == '"') {
            Some(i) => (&line[..i], line[i..].trim_start()),
            None => (line, ""),
        };
        let next = match keyword {
            "" if line.starts_with('"') => {
                let f = field.ok_or_else(bad)?;
                append(&mut entry, f, &unquote(line).ok_or_else(bad)?);
                continue;
            }
            "msgctxt" => Field::Ctx,
            "msgid" => Field::Id,
            "msgid_plural" => Field::IdPlural,
            "msgstr" => Field::Str(0),
            k if k.starts_with("msgstr[") && k.ends_with(']') => Field::Str(k[7..k.len() - 1].parse().map_err(|_| bad())?),
            _ => return Err(bad()),
        };
        if matches!(next, Field::Ctx | Field::Id) && started && field.is_some_and(|f| matches!(f, Field::Str(_))) {
            push(&mut entries, &mut entry, &mut started);
        }
        if !started {
            entry.fuzzy = std::mem::take(&mut flags_fuzzy);
            started = true;
        }
        field = Some(next);
        if let Field::Str(i) = next {
            if entry.forms.len() <= i {
                entry.forms.resize(i + 1, String::new());
            }
        }
        append(&mut entry, next, &unquote(rest).ok_or_else(bad)?);
        fn append(entry: &mut Entry, f: Field, s: &str) {
            match f {
                Field::Ctx => entry.ctx += s,
                Field::Id => entry.msgid += s,
                Field::IdPlural => {}
                Field::Str(i) => entry.forms[i] += s,
            }
        }
    }
    push(&mut entries, &mut entry, &mut started);
    Ok(entries)
}

/// A "quoted" .po string with its escapes undone.
fn unquote(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            return None;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next()? {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '"' => '"',
            '\\' => '\\',
            _ => return None,
        });
    }
    Some(out)
}

/// A Plural-Forms expression (C syntax over n), checked when read and evaluated without panicking.
#[derive(Debug)]
enum Plural {
    N,
    Num(i64),
    Not(Box<Plural>),
    Bin(char, Box<Plural>, Box<Plural>),
    And(Box<Plural>, Box<Plural>),
    Or(Box<Plural>, Box<Plural>),
    Cmp(&'static str, Box<Plural>, Box<Plural>),
    If(Box<Plural>, Box<Plural>, Box<Plural>),
}

impl Plural {
    fn parse(expr: &str) -> Result<Plural, String> {
        let tokens = tokenize(expr)?;
        let mut p = Parser { tokens, at: 0 };
        let tree = p.ternary()?;
        if p.at != p.tokens.len() {
            return Err(format!("the plural rule {expr:?} has more after its end"));
        }
        Ok(tree)
    }

    fn eval(&self, n: i64) -> Option<i64> {
        Some(match self {
            Plural::N => n,
            Plural::Num(v) => *v,
            Plural::Not(a) => (a.eval(n)? == 0) as i64,
            Plural::Bin(op, a, b) => {
                let (a, b) = (a.eval(n)?, b.eval(n)?);
                match op {
                    '+' => a.checked_add(b)?,
                    '-' => a.checked_sub(b)?,
                    '*' => a.checked_mul(b)?,
                    '/' => a.checked_div(b)?,
                    _ => a.checked_rem(b)?,
                }
            }
            Plural::And(a, b) => (a.eval(n)? != 0 && b.eval(n)? != 0) as i64,
            Plural::Or(a, b) => (a.eval(n)? != 0 || b.eval(n)? != 0) as i64,
            Plural::Cmp(op, a, b) => {
                let (a, b) = (a.eval(n)?, b.eval(n)?);
                (match *op { "==" => a == b, "!=" => a != b, "<" => a < b, ">" => a > b, "<=" => a <= b, _ => a >= b }) as i64
            }
            Plural::If(c, a, b) => if c.eval(n)? != 0 { a.eval(n)? } else { b.eval(n)? },
        })
    }
}

fn tokenize(expr: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let b: Vec<char> = expr.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push(b[start..i].iter().collect());
        } else if i + 1 < b.len() && ["==", "!=", "<=", ">=", "&&", "||"].contains(&format!("{c}{}", b[i + 1]).as_str()) {
            out.push(format!("{c}{}", b[i + 1]));
            i += 2;
        } else if "n+-*/%<>!?:()".contains(c) {
            out.push(c.to_string());
            i += 1;
        } else {
            return Err(format!("the plural rule {expr:?} has {c:?} in it"));
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<String>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(String::as_str)
    }

    fn eat(&mut self, t: &str) -> bool {
        let yes = self.peek() == Some(t);
        self.at += yes as usize;
        yes
    }

    fn ternary(&mut self) -> Result<Plural, String> {
        let cond = self.or()?;
        if !self.eat("?") {
            return Ok(cond);
        }
        let a = self.ternary()?;
        if !self.eat(":") {
            return Err("a ? in the plural rule has no :".into());
        }
        Ok(Plural::If(Box::new(cond), Box::new(a), Box::new(self.ternary()?)))
    }

    fn or(&mut self) -> Result<Plural, String> {
        let mut a = self.and()?;
        while self.eat("||") {
            a = Plural::Or(Box::new(a), Box::new(self.and()?));
        }
        Ok(a)
    }

    fn and(&mut self) -> Result<Plural, String> {
        let mut a = self.cmp()?;
        while self.eat("&&") {
            a = Plural::And(Box::new(a), Box::new(self.cmp()?));
        }
        Ok(a)
    }

    fn cmp(&mut self) -> Result<Plural, String> {
        let mut a = self.sum()?;
        while let Some(op) = ["==", "!=", "<=", ">=", "<", ">"].into_iter().find(|op| self.peek() == Some(op)) {
            self.at += 1;
            a = Plural::Cmp(op, Box::new(a), Box::new(self.sum()?));
        }
        Ok(a)
    }

    fn sum(&mut self) -> Result<Plural, String> {
        let mut a = self.product()?;
        while let Some(op) = ['+', '-'].into_iter().find(|op| self.peek() == Some(&op.to_string())) {
            self.at += 1;
            a = Plural::Bin(op, Box::new(a), Box::new(self.product()?));
        }
        Ok(a)
    }

    fn product(&mut self) -> Result<Plural, String> {
        let mut a = self.unary()?;
        while let Some(op) = ['*', '/', '%'].into_iter().find(|op| self.peek() == Some(&op.to_string())) {
            self.at += 1;
            a = Plural::Bin(op, Box::new(a), Box::new(self.unary()?));
        }
        Ok(a)
    }

    fn unary(&mut self) -> Result<Plural, String> {
        if self.eat("!") {
            return Ok(Plural::Not(Box::new(self.unary()?)));
        }
        if self.eat("(") {
            let inner = self.ternary()?;
            return if self.eat(")") { Ok(inner) } else { Err("a ( in the plural rule isn't closed".into()) };
        }
        if self.eat("n") {
            return Ok(Plural::N);
        }
        let token = self.peek().ok_or("the plural rule ends too soon")?.to_string();
        let v = token.parse().map_err(|_| format!("{token:?} in the plural rule isn't a number"))?;
        self.at += 1;
        Ok(Plural::Num(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n\n";
    const SV: &str = "msgid \"Hello\"\nmsgstr \"Hej\"\n\nmsgid \"{n} cat\"\nmsgid_plural \"{n} cats\"\nmsgstr[0] \"{n} katt\"\n\
        msgstr[1] \"{n} katter\"\n\nmsgid \"Hi {who}\"\nmsgstr \"Hej {who}\"\n";

    fn catalog(body: &str) -> Result<Catalog, String> {
        Catalog::parse(format!("{HEADER}{body}").as_bytes())
    }

    #[test]
    fn a_catalog_translates_plain_plural_and_filled_texts() {
        let c = catalog(SV).unwrap();
        assert_eq!(tr(Some(&c), "Hello", "", &[]), "Hej");
        assert_eq!(trn(Some(&c), "{n} cat", "{n} cats", 2, "", &[]), "2 katter");
        assert_eq!(trn(Some(&c), "{n} cat", "{n} cats", 1, "", &[]), "1 katt");
        assert_eq!(tr(Some(&c), "Hi {who}", "", &[("who", &"you")]), "Hej you");
        assert_eq!(tr(Some(&c), "Not translated", "", &[]), "Not translated");
    }

    #[test]
    fn broken_files_are_errors() {
        for broken in ["msgid \"Hello\"\nnonsense\n", "msgid \"Hello\"\nmsgstr \"Hej\"\n\"unfinished\n", "msgid \"{n} cat\"\nmsgid_plural \"{n} cats\"\nmsgstr[x] \"a\"\n"] {
            assert!(catalog(broken).is_err(), "{broken:?}");
        }
        assert!(Catalog::parse(b"\xff\xfe\x00 not text").is_err());
    }

    #[test]
    fn plural_rules_evaluate_like_c() {
        let ru = Plural::parse("(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2)").unwrap();
        let forms: Vec<i64> = [1, 2, 5, 11, 21, 22].iter().map(|n| ru.eval(*n).unwrap()).collect();
        assert_eq!(forms, [0, 1, 2, 2, 0, 1]);
        assert_eq!(Plural::parse("n % 0").unwrap().eval(2), None);
        assert!(Plural::parse("n +").is_err() && Plural::parse("import os").is_err());
    }
}
