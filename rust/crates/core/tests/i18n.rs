//! core::i18n against telescope/i18n.py and tests/test_i18n.py: the catalogs load as Python loads them, a translation can't break a
//! text, and the language follows the system unless one was picked. fixtures/i18n.json is what Python made
//! of the real catalogs. The language is global, so every test holds the lock.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use telescope_core::i18n::{self, lang, po, Fields};

static LOCK: Mutex<()> = Mutex::new(());

const ENV: [&str; 5] = ["LANG", "LANGUAGE", "LC_ALL", "LC_MESSAGES", i18n::ENV_LANGUAGE];

/// Takes the lock, and leaves English and a clean environment behind afterwards.
struct Quiet<'a>(#[allow(dead_code)] MutexGuard<'a, ()>);

impl Drop for Quiet<'_> {
    fn drop(&mut self) {
        for name in ENV {
            std::env::remove_var(name);
        }
        i18n::load(Some("en"));
    }
}

fn quiet() -> Quiet<'static> {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for name in ENV {
        std::env::remove_var(name);
    }
    Quiet(guard)
}

fn real_locale() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locale")
}

/// A locale folder of the test's own.
fn catalogs() -> PathBuf {
    let dir = telescope_core::testing::scratch_dir("locale");
    i18n::use_folder(&dir);
    dir
}

fn write_po(folder: &Path, lang: &str, body: &str, plural: &str) {
    let header = format!("msgid \"\"\nmsgstr \"\"\n\"Language: {lang}\\n\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Plural-Forms: {plural}\\n\"\n\n");
    std::fs::write(folder.join(format!("{lang}.po")), header + body).unwrap();
}

fn po_file(folder: &Path, lang: &str, body: &str) {
    write_po(folder, lang, body, "nplurals=2; plural=(n != 1);");
}

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/i18n.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("i18n.json")).expect("json")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
}

// ── What the real catalogs are made of ──────────────────────────────────────

#[test]
fn every_catalog_loads_with_its_plural_rule_name_and_number_style() {
    let _q = quiet();
    let data = fixture();
    i18n::use_folder(real_locale());
    assert_eq!(i18n::available(), strings(&data["available"]));
    for (code, want) in data["languages"].as_object().unwrap() {
        assert_eq!(i18n::load(Some(code)), *code);
        let bytes = std::fs::read(real_locale().join(format!("{code}.po"))).unwrap();
        let catalog = i18n::Catalog::from_po(&bytes).unwrap();
        assert!(catalog.len() > 1000, "{code}: only {} texts", catalog.len());
        let (_, header) = po::read_po(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(po::plural_count(&header) as u64, want["nplurals"].as_u64().unwrap(), "{code}: plural forms");
        let rule = po::plural_rule(&header);
        for (n, form) in want["forms"].as_array().unwrap().iter().enumerate() {
            assert_eq!(rule.eval(n as i64), form.as_i64(), "{code}: the plural form for {n}");
            assert_eq!(catalog.form(n as i64).map(|f| f as i64), form.as_i64());
        }
        assert_eq!(native_name_of(code), want["name"].as_str().unwrap(), "{code}: its name");
        let marks = lang::marks(code);
        assert_eq!([marks.0, marks.1], [want["marks"][0].as_str().unwrap(), want["marks"][1].as_str().unwrap()], "{code}: number marks");
        for n in want["numbers"].as_array().unwrap() {
            assert_eq!(i18n::number(n[0].as_f64().unwrap(), n[1].as_u64().unwrap() as usize, n[2] == true), n[3].as_str().unwrap(), "{code}: {n}");
        }
        for p in want["percents"].as_array().unwrap() {
            assert_eq!(i18n::percent(p[0].as_f64().unwrap(), p[1].as_u64().unwrap() as usize, p[2] == true), p[3].as_str().unwrap(), "{code}: {p}");
        }
    }
}

fn native_name_of(code: &str) -> String {
    i18n::native_name(code)
}

#[cfg(not(windows))] // the system language comes from the environment
#[test]
fn the_language_is_chosen_as_python_chooses_it() {
    let _q = quiet();
    let data = fixture();
    i18n::use_folder(real_locale());
    for case in data["resolve"].as_array().unwrap() {
        for name in ENV {
            std::env::remove_var(name);
        }
        for (name, value) in case["env"].as_object().unwrap() {
            std::env::set_var(name, value.as_str().unwrap());
        }
        let chosen = case["chosen"].as_str().unwrap();
        assert_eq!(i18n::resolve(chosen), case["result"].as_str().unwrap(), "{case}");
    }
    for case in data["pick"].as_array().unwrap() {
        let (wanted, have) = (strings(&case["wanted"]), strings(&case["have"]));
        assert_eq!(i18n::pick(&wanted, &have), case["result"].as_str().unwrap(), "{case}");
    }
}

#[test]
fn the_pseudo_language_writes_what_python_writes() {
    for pair in fixture()["pseudo"].as_array().unwrap() {
        assert_eq!(i18n::pseudo(pair[0].as_str().unwrap()), pair[1].as_str().unwrap(), "{pair}");
    }
}

// test_i18n.py::test_each_language_has_the_template_and_working_fields
#[test]
fn each_language_has_the_template_and_working_fields() {
    let template = std::fs::read_to_string(real_locale().join("telescope.pot")).unwrap();
    let (template, _) = po::read_po(&template).unwrap();
    let want: Vec<String> = template.iter().map(po::Entry::key).collect();
    let mut found = 0;
    for file in std::fs::read_dir(real_locale()).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "po")) {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let (entries, header) = po::read_po(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let have: Vec<String> = entries.iter().map(po::Entry::key).collect();
        let (mut a, mut b) = (want.clone(), have);
        a.sort();
        b.sort();
        assert_eq!(a, b, "{name} is out of date: run `python tools/i18n.py extract` in rust/ and commit locale");
        let count = po::plural_count(&header);
        assert!(po::plural_rule(&header).eval(1).unwrap() < count as i64, "{name}: its rule gives the singular a form it hasn't");
        for e in &entries {
            let mut allowed = po::fields(&e.msgid);
            allowed.extend(po::fields(&e.plural));
            for text in &e.msgstr {
                assert!(po::fields(text).iter().all(|f| allowed.contains(f)), "{name}: {:?} has a {{field}} the English doesn't", e.msgid);
            }
        }
        found += 1;
    }
    assert_eq!(found, 16);
}

// ── Looking texts up ────────────────────────────────────────────────────────

// test_i18n.py::test_english_is_what_the_code_says
#[test]
fn english_is_what_the_code_says() {
    let _q = quiet();
    i18n::load(Some("en"));
    assert_eq!(i18n::tr_with("Paired with {name}", "", &[("name", &"Pixel")]), "Paired with Pixel");
    assert_eq!(i18n::trn("{n} phone", "{n} phones", 1), "1 phone");
    assert_eq!(i18n::trn("{n} phone", "{n} phones", 3), "3 phones");
}

// test_i18n.py::test_a_translation_is_used_with_its_fields_and_plural_forms
#[test]
fn a_translation_is_used_with_its_fields_and_plural_forms() {
    let _q = quiet();
    let folder = catalogs();
    po_file(&folder, "sv", "msgid \"Paired with {name}\"\nmsgstr \"Parkopplad med {name}\"\n\nmsgid \"{n} phone\"\nmsgid_plural \"{n} phones\"\n\
        msgstr[0] \"{n} telefon\"\nmsgstr[1] \"{n} telefoner\"\n\nmsgctxt \"search\"\nmsgid \"audio sound\"\nmsgstr \"ljud\"\n");
    assert_eq!(i18n::load(Some("sv")), "sv");
    assert_eq!(i18n::tr_with("Paired with {name}", "", &[("name", &"Pixel")]), "Parkopplad med Pixel");
    assert_eq!(i18n::trn("{n} phone", "{n} phones", 1), "1 telefon");
    assert_eq!(i18n::trn("{n} phone", "{n} phones", 2), "2 telefoner");
    assert_eq!(i18n::tr_ctx("search", "audio sound"), "ljud");
    assert_eq!(i18n::tr("audio sound"), "audio sound"); // another context
    assert_eq!(i18n::tr("Not translated yet"), "Not translated yet");
}

// What str.format makes of a field's conversion and format spec, recorded from Python
#[test]
fn a_field_takes_the_format_specs_python_does() {
    let data = fixture();
    for case in data["formats"].as_array().unwrap() {
        let text = case[0].as_str().unwrap();
        let value = match &case[1] {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let fields: [(&str, &dyn std::fmt::Display); 2] = [("n", &value), ("name", &value)];
        assert_eq!(i18n::fill(text, &fields).ok().as_deref(), case[2].as_str(), "{case}");
    }
}

// A field among code's own blocks is still found, and what isn't a field is left alone
#[test]
fn one_field_at_a_time_leaves_everything_else() {
    assert_eq!("a { b: {left}; } {right} {{left}}".field("left", 1), "a { b: 1; } {right} {{left}}");
    assert_eq!("{n:02d} of {all}".field("n", 5).field("all", 9), "05 of 9");
    assert_eq!("{n:02d}".field("n", "x"), "x", "a spec the value can't take");
}

// A broken plural form falls back on its own, the others stay (i18n.py's trn), and format specs are kept
#[test]
fn a_broken_plural_form_shows_the_english_for_that_form_alone() {
    let po = r#"msgid ""
msgstr ""
"Content-Type: text/plain; charset=UTF-8\n"
"Plural-Forms: nplurals=2; plural=(n != 1);\n"

msgid "{n} phone"
msgid_plural "{n} phones"
msgstr[0] "{n} telefon"
msgstr[1] "{gone} telefoner"

msgid "{n} cam"
msgid_plural "{n} cams"
msgstr[0] "{n:02d} kamera"
msgstr[1] "{n:02d} kameror"

msgid "Took {seconds} s"
msgstr "{seconds:.1f} s"
"#;
    let catalog = i18n::Catalog::from_po(po.as_bytes()).unwrap();
    assert_eq!(catalog.get("{n} phone").unwrap(), ["{n} telefon", ""]);
    assert_eq!(catalog.get("{n} cam").unwrap(), ["{n:02d} kamera", "{n:02d} kameror"]);
    assert_eq!(catalog.get("Took {seconds} s").unwrap(), ["{seconds:.1f} s"]);
    assert_eq!(catalog.get("Took {seconds} s").unwrap()[0].field("seconds", 2.25), "2.2 s");
}

// test_i18n.py::test_a_broken_or_unfinished_translation_shows_the_english
#[test]
fn a_broken_or_unfinished_translation_shows_the_english() {
    let _q = quiet();
    let folder = catalogs();
    po_file(&folder, "sv", "msgid \"Paired with {name}\"\nmsgstr \"Parkopplad med {namn}\"\n\n#, fuzzy\nmsgid \"Remove\"\nmsgstr \"Ta bort\"\n\n\
        msgid \"Cancel\"\nmsgstr \"\"\n\nmsgid \"Stray {\"\nmsgstr \"Lös { klammer\"\n");
    i18n::load(Some("sv"));
    assert_eq!(i18n::tr_with("Paired with {name}", "", &[("name", &"Pixel")]), "Paired with Pixel");
    // Looked up and filled apart, as the code that fills a text itself does
    assert_eq!(i18n::tr("Paired with {name}"), "Paired with {name}");
    assert_eq!(i18n::tr("Remove"), "Remove");
    assert_eq!(i18n::tr("Cancel"), "Cancel");
    assert_eq!(i18n::tr("Stray {"), "Stray {");
}

// test_i18n.py::test_a_language_with_three_plural_forms
#[test]
fn a_language_with_three_plural_forms() {
    let _q = quiet();
    let folder = catalogs();
    write_po(&folder, "pl", "msgid \"{n} phone\"\nmsgid_plural \"{n} phones\"\nmsgstr[0] \"{n} telefon\"\nmsgstr[1] \"{n} telefony\"\nmsgstr[2] \"{n} telefonów\"\n",
        "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);");
    i18n::load(Some("pl"));
    let shown: Vec<String> = [1, 3, 5, 22].iter().map(|n| i18n::trn("{n} phone", "{n} phones", *n)).collect();
    assert_eq!(shown, ["1 telefon", "3 telefony", "5 telefonów", "22 telefony"]);
}

// test_i18n.py::test_a_text_keeps_its_english_when_another_is_translated_alike
#[test]
fn a_text_keeps_its_english_when_another_is_translated_alike() {
    let _q = quiet();
    let folder = catalogs();
    // Python's tr() hands back a text that remembers its English. Here the code that keeps a key holds the English itself (a fold is
    // saved under "camera/Light", whatever its title shows), and english() looks up any other text, the first English for "Bild"
    po_file(&folder, "sv", "msgid \"Picture\"\nmsgstr \"Bild\"\n\nmsgid \"Image\"\nmsgstr \"Bild\"\n");
    i18n::load(Some("sv"));
    assert_eq!((i18n::tr("Picture"), i18n::tr("Image")), ("Bild".into(), "Bild".into()));
    assert_eq!(i18n::english("Bild"), "Picture");
    // Text that didn't come from a translation is itself
    assert_eq!(i18n::english("Ljus"), "Ljus");
}

// test_i18n.py::test_a_list_reads_naturally
#[test]
fn a_list_reads_naturally() {
    let _q = quiet();
    i18n::load(Some("en"));
    let list = |words: &[&str]| i18n::and_list(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
    assert_eq!(list(&["Camera"]), "Camera");
    assert_eq!(list(&["Camera", "Microphone", "Preview"]), "Camera, Microphone and Preview");
}

// test_i18n.py::test_shown_numbers_follow_the_language
#[test]
fn shown_numbers_follow_the_language() {
    let _q = quiet();
    let folder = catalogs();
    i18n::load(Some("en"));
    assert_eq!(i18n::number(1.0, 1, false), "1.0");
    assert_eq!(i18n::number(-0.5, 1, true), "-0.5");
    assert_eq!(i18n::number(10.0, 0, true), "+10");
    assert_eq!(i18n::percent(80.0, 0, false), "80%");
    assert_eq!(i18n::percent(-10.0, 0, true), "-10%");
    po_file(&folder, "sv", "msgctxt \"percentage\"\nmsgid \"{n}%\"\nmsgstr \"{n} %\"\n");
    assert_eq!(i18n::load(Some("sv")), "sv");
    assert_eq!(i18n::number(1.0, 1, false), "1,0");
    assert_eq!(i18n::number(45.5, 1, false), "45,5");
    assert_eq!(i18n::number(10.0, 0, true), "+10");
    assert_eq!(i18n::percent(80.0, 0, false), "80 %");
    assert_eq!(i18n::percent(12.5, 1, false), "12,5 %");
    i18n::load(Some("en"));
    assert_eq!(i18n::number(1.0, 1, false), "1.0");
}

// test_i18n.py::test_the_language_follows_the_system_unless_set
#[test]
fn the_language_follows_the_system_unless_set() {
    let _q = quiet();
    let folder = catalogs();
    po_file(&folder, "sv", "");
    po_file(&folder, "pt_BR", "");
    assert_eq!(i18n::available(), ["pt_BR", "sv"]);
    let have = i18n::available();
    let pick = |w: &[&str]| i18n::pick(&w.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &have);
    assert_eq!(pick(&["sv-SE", "en-US"]), "sv");
    assert_eq!(pick(&["pt-BR"]), "pt_BR");
    assert_eq!(pick(&["de-DE", "en-GB", "sv"]), "en"); // English comes before Swedish there
    assert_eq!(pick(&["fr"]), "en");
    std::env::set_var(i18n::ENV_LANGUAGE, "sv");
    assert_eq!(i18n::load(None), "sv");
}

// test_i18n.py::test_the_language_picked_in_settings_wins_over_the_system
#[cfg(not(windows))] // the system language comes from the environment
#[test]
fn the_language_picked_in_settings_wins_over_the_system() {
    let _q = quiet();
    let folder = catalogs();
    po_file(&folder, "sv", "");
    std::env::set_var("LANG", "sv_SE.UTF-8");
    assert_eq!(i18n::resolve(""), "sv");
    assert_eq!(i18n::resolve("en"), "en");
    assert_eq!(i18n::resolve("de"), "en"); // a language that's gone since it was picked
    std::env::set_var(i18n::ENV_LANGUAGE, "en");
    assert_eq!(i18n::resolve("sv"), "en");
    assert_eq!(i18n::native_name("sv"), "Svenska");
    assert_eq!(i18n::native_name("en"), "English");
}

// test_i18n.py::test_a_first_draft_says_so
#[test]
fn a_first_draft_says_so() {
    let _q = quiet();
    let folder = catalogs();
    po_file(&folder, "sv", "");
    write_po(&folder, "es", "", "nplurals=2; plural=(n != 1);\\n\"\n\"X-Telescope-Draft: yes");
    assert!(i18n::is_draft("es"));
    assert!(!i18n::is_draft("sv"));
    assert!(!i18n::is_draft("en"));
    assert!(!i18n::is_draft("de")); // no catalog at all
}

// test_i18n.py::test_an_unreadable_catalog_leaves_english
#[test]
fn an_unreadable_catalog_leaves_english() {
    let _q = quiet();
    let folder = catalogs();
    std::fs::write(folder.join("sv.po"), "msgid \"x\"\nnonsense\n").unwrap();
    assert_eq!(i18n::load(Some("sv")), "en");
    assert_eq!(i18n::language(), "en");
}

// test_i18n.py::test_a_field_can_have_any_name
#[test]
fn a_field_can_have_any_name() {
    let _q = quiet();
    i18n::load(Some("en"));
    assert_eq!(i18n::tr_with("Nothing matches {text}", "", &[("text", &"zz")]), "Nothing matches zz");
    assert_eq!(i18n::trn_with("{n} of {singular}", "{n} of {singular}", 2, "", &[("singular", &"x")]), "2 of x");
}

// test_i18n.py::test_the_pseudo_language_makes_every_text_longer_and_keeps_what_the_app_fills_in
#[test]
fn the_pseudo_language_makes_every_text_longer_and_keeps_what_the_app_fills_in() {
    let shown = i18n::pseudo("Made by <a href=\"{url}\">LunarKittyy</a>\nsecond line");
    let (first, second) = shown.split_once('\n').unwrap();
    assert!(first.contains("<a href=\"{url}\">") && first.contains("</a>") && !first.contains("LunarKittyy"));
    assert!(first.starts_with('[') && second.starts_with('[') && second.ends_with("~]"));
    assert!(i18n::pseudo("Start streaming").chars().count() as f64 >= "Start streaming".len() as f64 * 1.4);
}

// test_i18n.py::test_the_pseudo_language_can_be_picked_to_try_a_layout
#[test]
fn the_pseudo_language_can_be_picked_to_try_a_layout() {
    let _q = quiet();
    i18n::use_folder(real_locale());
    std::env::set_var(i18n::ENV_LANGUAGE, i18n::PSEUDO_LANGUAGE);
    assert_eq!(i18n::load(Some(&i18n::resolve(""))), i18n::PSEUDO_LANGUAGE);
    assert_eq!(i18n::tr("Start streaming"), i18n::pseudo("Start streaming"));
    assert_eq!(i18n::english(&i18n::tr("Start streaming")), "Start streaming");
    assert!(!i18n::available().iter().any(|l| l == i18n::PSEUDO_LANGUAGE)); // not in the Language list
}
