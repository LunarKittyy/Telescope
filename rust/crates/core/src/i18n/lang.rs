//! Which language to load: the one picked in Settings, the system's, or TELESCOPE_LANG (telescope.i18n's pick, resolve and
//! system_languages), and what Settings calls each language.

/// The source language, which is what the code says.
pub const SOURCE_LANGUAGE: &str = "en";
/// Overrides the system language and the one picked in Settings, e.g. TELESCOPE_LANG=sv to try a translation.
pub const ENV_LANGUAGE: &str = "TELESCOPE_LANG";
/// A made-up language: every text longer and accented, so a layout with no room for a translation shows.
pub const PSEUDO_LANGUAGE: &str = "pseudo";

/// The first wanted language there's a catalog for: "pt-BR" takes pt_BR, then pt. English if none.
pub fn pick(wanted: &[String], have: &[String]) -> String {
    for tag in wanted {
        let tag = tag.replace('-', "_");
        if tag == PSEUDO_LANGUAGE {
            return tag;
        }
        let base = tag.split('_').next().unwrap_or("");
        if base == SOURCE_LANGUAGE {
            return SOURCE_LANGUAGE.into();
        }
        for candidate in [tag.as_str(), base] {
            if have.iter().any(|h| h == candidate) {
                return candidate.into();
            }
        }
    }
    SOURCE_LANGUAGE.into()
}

/// The language to load for the one picked in Settings ("" is the system's). TELESCOPE_LANG wins over both.
pub fn resolve(chosen: &str, have: &[String]) -> String {
    let over = std::env::var(ENV_LANGUAGE).unwrap_or_default();
    let over = over.trim();
    let asked = if over.is_empty() { chosen } else { over };
    if asked.is_empty() {
        pick(&system_languages(), have)
    } else {
        pick(&[asked.to_string()], have)
    }
}

/// What Qt lists for a language tag: the tag itself, then without its script, then the language alone, so "zh-Hans-CN" also
/// offers "zh-CN" and a bare "pt" or "zh" offers the region Qt guesses for it.
pub fn expand(tag: &str) -> Vec<String> {
    let parts: Vec<&str> = tag.split(['-', '_']).filter(|p| !p.is_empty()).collect();
    let Some(&written) = parts.first() else { return Vec::new() };
    // Qt's old language codes: it offers the current one, and the old one last
    let language = match written {
        "in" => "id",
        "iw" => "he",
        "ji" => "yi",
        "mo" => "ro",
        "no" => "nb",
        "sh" => "sr",
        "tl" => "fil",
        other => other,
    };
    let script = parts.iter().skip(1).find(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_alphabetic())).copied();
    let region = parts.iter().skip(1).find(|p| (p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic())) || (p.len() == 3 && p.chars().all(|c| c.is_ascii_digit()))).copied();
    // CLDR's likely regions for the languages whose catalog is for one region
    let likely = match (language, script) {
        ("pt", None | Some("Latn")) => Some("BR"),
        ("zh", None | Some("Hans")) => Some("CN"),
        _ => None,
    };
    let region = region.or(likely);
    let mut out = Vec::new();
    if let (Some(s), Some(r)) = (script, region) {
        out.push(format!("{language}-{s}-{r}"));
    }
    if let Some(r) = region {
        out.push(format!("{language}-{r}"));
    }
    if let Some(s) = script {
        out.push(format!("{language}-{s}"));
    }
    out.push(language.to_string());
    if written != language {
        out.push(written.to_string());
    }
    out
}

/// Takes the language part of a locale name as the environment writes it: "sv_SE.UTF-8@euro" is "sv_SE", "C" and "POSIX" are none.
#[cfg(not(windows))]
fn from_environment(value: &str) -> Option<String> {
    let name = value.split(['.', '@']).next().unwrap_or("").trim();
    (!name.is_empty() && name != "C" && name != "POSIX").then(|| name.to_string())
}

/// The languages the desktop asks for, most wanted first.
pub fn system_languages() -> Vec<String> {
    let names = system_locale_names();
    names.iter().flat_map(|n| expand(n)).collect()
}

#[cfg(not(windows))]
fn system_locale_names() -> Vec<String> {
    // Qt's order: LANGUAGE's list when set, else the first of LC_ALL, LC_MESSAGES and LANG
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    if let Some(list) = var("LANGUAGE") {
        return list.split(':').filter_map(from_environment).collect();
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|n| var(n)).and_then(|v| from_environment(&v)).into_iter().collect()
}

/// Qt's order: the list in Windows' language settings, which still names a language Windows itself has no display pack for,
/// then the display languages when that list can't be read.
#[cfg(windows)]
fn system_locale_names() -> Vec<String> {
    let listed = settings_languages();
    if listed.is_empty() { display_languages() } else { listed }
}

#[cfg(windows)]
fn settings_languages() -> Vec<String> {
    use windows::System::UserProfile::GlobalizationPreferences;
    let Ok(list) = GlobalizationPreferences::Languages() else { return Vec::new() };
    (0..list.Size().unwrap_or(0)).filter_map(|i| list.GetAt(i).ok()).map(|tag| tag.to_string()).filter(|t| !t.is_empty()).collect()
}

#[cfg(windows)]
fn display_languages() -> Vec<String> {
    extern "system" {
        fn GetUserPreferredUILanguages(flags: u32, count: *mut u32, buffer: *mut u16, size: *mut u32) -> i32;
    }
    const MUI_LANGUAGE_NAME: u32 = 0x8;
    let (mut count, mut size) = (0u32, 0u32);
    // SAFETY: the first call only asks how much room the list needs; the second fills a buffer of that size
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, std::ptr::null_mut(), &mut size) == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; size as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buffer.as_mut_ptr(), &mut size) == 0 {
            return Vec::new();
        }
        buffer.truncate(size as usize);
        String::from_utf16_lossy(&buffer).split('\0').filter(|t| !t.is_empty()).map(str::to_string).collect()
    }
}

/// A language's name in itself, as Settings lists it: "Svenska" for sv. Where Qt's own name is off, the one the Python app puts in
/// its place ("es" alone is "español de España", "id" is just "Indonesia").
pub fn native_name(lang: &str) -> String {
    match lang {
        "en" => "English",
        "de" => "Deutsch",
        "es" => "Español",
        "fr" => "Français",
        "hi" => "हिन्दी",
        "id" => "Bahasa Indonesia",
        "it" => "Italiano",
        "ja" => "日本語",
        "ko" => "한국어",
        "pl" => "Polski",
        "pt_BR" => "Português do Brasil",
        "ru" => "Русский",
        "sv" => "Svenska",
        "tr" => "Türkçe",
        "uk" => "Українська",
        "vi" => "Tiếng Việt",
        "zh_CN" => "简体中文",
        other => other,
    }
    .into()
}

/// (decimal point, minus sign) of a language, as Qt's locale gives them; English and anything unknown write 1.5 and -1.
pub fn marks(language: &str) -> (&'static str, &'static str) {
    match language.split(['_', '-']).next().unwrap_or("") {
        "de" | "es" | "fr" | "id" | "it" | "pl" | "pt" | "ru" | "tr" | "uk" | "vi" => (",", "-"),
        "sv" => (",", "\u{2212}"),
        _ => (".", "-"),
    }
}

#[cfg(all(test, windows))]
mod system_language {
    #[test]
    fn windows_says_which_languages_this_user_wants() {
        let names = super::system_locale_names();
        assert!(!names.is_empty() && names[0].contains('-'), "{names:?}");
        assert!(!super::system_languages().is_empty());
    }
}
