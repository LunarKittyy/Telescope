//! Shown text: Telescope's own tr() with Python's {field} filling, which core's tr() leaves to the caller.

use std::fmt::Display;

/// text with each {name} replaced by its value; {{ and }} stand for braces. A field with no value stays as it was.
pub fn fill(text: &str, fields: &[(&str, &dyn Display)]) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['{', '}']) {
        out += &rest[..i];
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push(tail.as_bytes()[0] as char);
            rest = &tail[2..];
            continue;
        }
        if tail.starts_with('}') {
            return Err(format!("a lone }} in {text:?}"));
        }
        let end = tail.find('}').ok_or_else(|| format!("an unclosed {{ in {text:?}"))?;
        let name = &tail[1..end];
        let (_, value) = fields.iter().find(|(n, _)| *n == name).ok_or_else(|| format!("no value for {{{name}}} in {text:?}"))?;
        out += &value.to_string();
        rest = &tail[end + 1..];
    }
    out += rest;
    Ok(out)
}

/// tr(msgid) with its fields filled in, falling back to the English when the translation's fields don't fit (i18n._fill).
pub fn tr(msgid: &str, fields: &[(&str, &dyn Display)]) -> String {
    let shown = telescope_core::i18n::tr(msgid);
    fill(&shown, fields).or_else(|_| fill(msgid, fields)).unwrap_or_else(|_| msgid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_fill_like_str_format() {
        assert_eq!(fill("It has no {file}.", &[("file", &"plugin.json")]).unwrap(), "It has no plugin.json.");
        assert_eq!(fill("{{n}} is {n}", &[("n", &3)]).unwrap(), "{n} is 3");
        assert!(fill("Hi {who}", &[]).is_err());
        assert_eq!(tr("Hi {who}", &[]), "Hi {who}");
    }
}
