//! .po files as telescope.i18n reads them: entries, the header and the plural rule. Reading is as forgiving as Python's
//! (an escape it doesn't know is the character itself) and as strict about lines that aren't .po.

/// One message of a .po file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub msgid: String,
    pub plural: String,
    pub ctx: String,
    /// One form for tr(), one per plural form for trn().
    pub msgstr: Vec<String>,
    /// "#." lines, from the code's "# Translators:" notes.
    pub comments: Vec<String>,
    /// "#:" file:line places.
    pub locations: Vec<String>,
    /// "#," flags, like fuzzy.
    pub flags: Vec<String>,
    pub obsolete: bool,
}

impl Entry {
    pub fn fuzzy(&self) -> bool {
        self.flags.iter().any(|f| f == "fuzzy")
    }

    /// What a translation is looked up by: the context and the English.
    pub fn key(&self) -> String {
        key(&self.ctx, &self.msgid)
    }
}

/// The lookup key of a text: the English alone, or "ctx\u{4}English" as gettext writes a text with a context.
pub fn key(ctx: &str, msgid: &str) -> String {
    if ctx.is_empty() { msgid.to_string() } else { format!("{ctx}\u{4}{msgid}") }
}

/// A quoted .po string with its escapes undone; \n, \t, \" and \\ mean what they say, any other escape is the character after it.
fn unquote(s: &str) -> Result<String, String> {
    let s = s.trim();
    let inner = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')).filter(|_| s.len() >= 2).ok_or_else(|| format!("not a quoted string: {s}"))?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('\\', Some(next)) => {
                chars.next();
                out.push(match next { 'n' => '\n', 't' => '\t', other => other });
            }
            _ => out.push(c),
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Ctx,
    Id,
    Plural,
    Str(usize),
}

/// The entries of a .po file, without the header and the obsolete ones, and the header (the msgstr of the empty msgid).
/// A line that isn't .po makes the whole file an error, which is how a broken catalog is told from an empty one.
pub fn read_po(text: &str) -> Result<(Vec<Entry>, String), String> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut entry = Entry::default();
    let mut field: Option<Field> = None;
    let mut started = false;
    let finish = |entries: &mut Vec<Entry>, entry: &mut Entry, started: &mut bool| {
        if *started {
            entries.push(std::mem::take(entry));
        }
        *entry = Entry::default();
        *started = false;
    };
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        // Python's str.splitlines also breaks on a few characters lines() doesn't; none are in a file a person wrote
        let mut line = raw.trim();
        let obsolete = line.starts_with("#~");
        if obsolete {
            line = line[2..].trim();
        }
        if line.is_empty() {
            finish(&mut entries, &mut entry, &mut started);
            field = None;
            continue;
        }
        if line.starts_with('#') {
            if started && field.is_some() {
                finish(&mut entries, &mut entry, &mut started);
                field = None;
            }
            if let Some(rest) = line.strip_prefix("#.") {
                entry.comments.push(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("#:") {
                entry.locations.extend(rest.split_whitespace().map(str::to_string));
            } else if let Some(rest) = line.strip_prefix("#,") {
                entry.flags.extend(rest.split(',').map(str::trim).filter(|f| !f.is_empty()).map(str::to_string));
            }
            continue;
        }
        if line.starts_with('"') {
            let Some(f) = field else { return Err(format!("line {number}: text outside a field")) };
            let text = unquote(line)?;
            match f {
                Field::Ctx => entry.ctx += &text,
                Field::Id => entry.msgid += &text,
                Field::Plural => entry.plural += &text,
                Field::Str(i) => entry.msgstr[i] += &text,
            }
            continue;
        }
        let (keyword, value) = line.split_once(' ').unwrap_or((line, ""));
        if (keyword == "msgctxt" || keyword == "msgid") && started && matches!(field, Some(Field::Str(_))) {
            finish(&mut entries, &mut entry, &mut started);
        }
        started = true;
        entry.obsolete = entry.obsolete || obsolete;
        match keyword {
            "msgctxt" => {
                entry.ctx = unquote(value)?;
                field = Some(Field::Ctx);
            }
            "msgid" => {
                entry.msgid = unquote(value)?;
                field = Some(Field::Id);
            }
            "msgid_plural" => {
                entry.plural = unquote(value)?;
                field = Some(Field::Plural);
            }
            k if k == "msgstr" || k.starts_with("msgstr[") => {
                let bad = || format!("line {number}: unknown line {raw:?}");
                let i = if k == "msgstr" { 0 } else { k.get(7..k.len().saturating_sub(1)).and_then(|s| s.parse::<usize>().ok()).ok_or_else(bad)? };
                // A form number is a count, never a size to allocate
                if i > 64 {
                    return Err(format!("line {number}: unknown line {raw:?}"));
                }
                if entry.msgstr.len() <= i {
                    entry.msgstr.resize(i + 1, String::new());
                }
                entry.msgstr[i] = unquote(value)?;
                field = Some(Field::Str(i));
            }
            _ => return Err(format!("line {number}: unknown line {raw:?}")),
        }
    }
    finish(&mut entries, &mut entry, &mut started);
    let mut header = String::new();
    for e in &entries {
        if e.msgid.is_empty() && e.ctx.is_empty() {
            header = e.msgstr.first().cloned().unwrap_or_default();
        }
    }
    Ok((entries.into_iter().filter(|e| !e.msgid.is_empty() && !e.obsolete).collect(), header))
}

/// A header's field ("Plural-Forms"), "" when it has none.
pub fn header_field(header: &str, name: &str) -> String {
    for line in header.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case(name) {
                return value.trim().to_string();
            }
        }
    }
    String::new()
}

/// How many plural forms the language has (2 when the header doesn't say).
pub fn plural_count(header: &str) -> usize {
    let field = header_field(header, "Plural-Forms");
    let Some(at) = field.find("nplurals") else { return 2 };
    let rest = field[at + 8..].trim_start();
    let digits: String = rest.strip_prefix('=').unwrap_or("").trim_start().chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(2)
}

/// The rule that picks a plural form from a count; English's when the header has none or its rule is broken.
pub fn plural_rule(header: &str) -> Plural {
    let field = header_field(header, "Plural-Forms");
    // The first "plural" with an = after it, not the one in "nplurals"
    let expr = field.match_indices("plural").find_map(|(at, _)| field[at + 6..].trim_start().strip_prefix('='));
    let Some(expr) = expr else { return Plural::english() };
    let expr = expr.trim();
    let expr = expr.strip_suffix(';').map(str::trim_end).unwrap_or(expr);
    Plural::parse(expr).unwrap_or_else(|e| {
        log::warn!("Bad Plural-Forms {expr:?}, using English's: {e}");
        Plural::english()
    })
}

/// The {fields} a text fills in.
pub fn fields(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let b: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '{' && b.get(i + 1) == Some(&'{') {
            i += 2;
            continue;
        }
        if b[i] == '{' {
            let start = i + 1;
            let mut end = start;
            // (?<!\{)\{([A-Za-z_][A-Za-z0-9_]*)(?:[:!][^{}]*)?\}(?!\})
            while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == '_') {
                end += 1;
            }
            let name: String = b[start..end].iter().collect();
            let named = !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit());
            let mut close = end;
            if named && matches!(b.get(close), Some(':' | '!')) {
                while close < b.len() && !matches!(b[close], '{' | '}') {
                    close += 1;
                }
            }
            if named && b.get(close) == Some(&'}') && b.get(close + 1) != Some(&'}') {
                if !out.contains(&name) {
                    out.push(name);
                }
                i = close + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// A Plural-Forms expression (C syntax over n), checked when read and evaluated without panicking.
#[derive(Clone, Debug)]
pub enum Plural {
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
    /// n != 1, the rule of a language that says nothing else.
    pub fn english() -> Plural {
        Plural::Cmp("!=", Box::new(Plural::N), Box::new(Plural::Num(1)))
    }

    pub fn parse(expr: &str) -> Result<Plural, String> {
        let tokens = tokenize(expr)?;
        let mut p = Parser { tokens, at: 0 };
        let tree = p.ternary()?;
        if p.at != p.tokens.len() {
            return Err(format!("the plural rule {expr:?} has more after its end"));
        }
        Ok(tree)
    }

    /// The form for n, None when the rule can't say (it divides by zero).
    pub fn eval(&self, n: i64) -> Option<i64> {
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
