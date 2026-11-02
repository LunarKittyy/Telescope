//! A plugin someone added can ship its own translations: locale/<lang>.po in its folder, used for its texts before Telescope's own, with
//! English for what's missing, and a file that's broken costs it nothing but the translation (test_plugin_i18n.py).

use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use telescope_core::testing::scratch_dir;
use telescope_plugin_host::testing::PluginTest;

const HEADER: &str = "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n\n";
const MAIN: &str = r#"local helper = require("helper")
local LATER = N_("Hello")

return {
  texts = function()
    return { tr("Hello"), tr("Not translated"), tr("Camera"), trn("{n} cat", "{n} cats", 2), tr(LATER),
             tr("Hi {who}", { who = "you" }), helper.hello() }
  end,
}
"#;
const HELPER: &str = "return { hello = function() return tr('Hello') end }\n";
const SV: &str = "msgid \"Hello\"\nmsgstr \"Hej\"\n\nmsgid \"{n} cat\"\nmsgid_plural \"{n} cats\"\nmsgstr[0] \"{n} katt\"\n\
    msgstr[1] \"{n} katter\"\n\nmsgid \"Hi {who}\"\nmsgstr \"Hej {who}\"\n";

thread_local! {
    /// The language Telescope itself shows in this test (core's translator is global, the tests run on their own threads).
    static LANGUAGE: RefCell<String> = RefCell::new("en".into());
}

/// Telescope's own Swedish knows Camera and its own Hello, as the fixture in the Python test does.
fn telescope_in(lang: &str) {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| telescope_core::i18n::set_translator(|msgid| {
        let sv = LANGUAGE.with(|l| l.borrow().clone()) == "sv";
        match msgid {
            "Camera" if sv => Some("Kamera".into()),
            "Hello" if sv => Some("Hallå från Telescope".into()),
            _ => None,
        }
    }));
    LANGUAGE.with(|l| *l.borrow_mut() = lang.into());
}

fn add(dir: &Path, name: &str, catalog: Option<&[u8]>, filename: &str) -> PathBuf {
    let target = dir.join(name);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("plugin.json"), json!({"name": name, "api": 2}).to_string()).unwrap();
    std::fs::write(target.join("main.lua"), MAIN).unwrap();
    std::fs::write(target.join("helper.lua"), HELPER).unwrap();
    if let Some(body) = catalog {
        std::fs::create_dir(target.join("locale")).unwrap();
        std::fs::write(target.join("locale").join(filename), body).unwrap();
    }
    target
}

fn po(body: &str) -> Vec<u8> {
    format!("{HEADER}{body}").into_bytes()
}

fn texts(folder: &Path, lang: &str) -> Vec<String> {
    telescope_in(lang);
    let mut t = PluginTest::open_in(folder, lang).unwrap();
    let Value::Array(list) = t.call("texts", vec![]).unwrap() else { panic!("texts() gave no list") };
    list.into_iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

#[test]
fn a_plugin_with_a_catalog_shows_its_texts_in_the_language() {
    let folder = add(&scratch_dir("i18n"), "cats", Some(&po(SV)), "sv.po");
    let t = texts(&folder, "sv");
    assert_eq!(t[0], "Hej", "the plugin's catalog beats Telescope's translation of the same English");
    assert_eq!(t[3], "2 katter");
    assert_eq!(t[4], "Hej", "a text marked with N_() when loading, translated where it's shown");
    assert_eq!(t[5], "Hej you");
    assert_eq!(t[6], "Hej", "from another module of the plugin");
    assert_eq!(telescope_core::i18n::tr("Hello"), "Hallå från Telescope", "Telescope's own code still uses its own catalog");
}

#[test]
fn a_plugin_without_a_catalog_stays_in_english_but_gets_telescopes_words() {
    let folder = add(&scratch_dir("i18n"), "cats", None, "");
    let t = texts(&folder, "sv");
    assert_eq!(t[0], "Hallå från Telescope", "Telescope's own translation of the same English");
    assert!(t[1] == "Not translated" && t[3] == "2 cats");
}

#[test]
fn a_plugin_with_another_language_only_has_english() {
    let folder = add(&scratch_dir("i18n"), "cats", Some(&po(SV)), "de.po");
    assert_eq!(texts(&folder, "sv")[3], "2 cats");
}

#[test]
fn a_text_the_plugin_hasnt_translated_falls_back_to_telescope_then_english() {
    let folder = add(&scratch_dir("i18n"), "cats", Some(&po(SV)), "sv.po");
    let t = texts(&folder, "sv");
    assert_eq!(t[1], "Not translated", "nobody has it: English");
    assert_eq!(t[2], "Kamera", "only Telescope has it");
}

#[test]
fn an_empty_or_fuzzy_translation_counts_as_missing() {
    let folder = add(&scratch_dir("i18n"), "cats", Some(&po("msgid \"Hello\"\nmsgstr \"\"\n\n#, fuzzy\nmsgid \"Not translated\"\nmsgstr \"Gammal\"\n")), "sv.po");
    let t = texts(&folder, "sv");
    assert!(t[0] == "Hallå från Telescope" && t[1] == "Not translated", "{t:?}");
}

#[test]
fn a_broken_file_leaves_the_plugin_in_english_and_still_loading() {
    let broken: [Vec<u8>; 4] = [po("msgid \"Hello\"\nnonsense\n"), po("msgid \"Hello\"\nmsgstr \"Hej\"\n\"unfinished\n"),
                                po("msgid \"{n} cat\"\nmsgid_plural \"{n} cats\"\nmsgstr[x] \"a\"\n"), b"\xff\xfe\x00 not text".to_vec()];
    for body in broken {
        let folder = add(&scratch_dir("i18n"), "cats", Some(&body), "sv.po");
        let t = texts(&folder, "sv");
        assert!(t[3] == "2 cats" && t[5] == "Hi you" && t[0] == "Hallå från Telescope", "{t:?}");
    }
}

#[test]
fn a_broken_plural_rule_or_field_shows_english_instead_of_failing() {
    let body = "msgid \"{n} cat\"\nmsgid_plural \"{n} cats\"\nmsgstr[0] \"a\"\nmsgstr[1] \"b\"\n\nmsgid \"Hi {who}\"\nmsgstr \"Hej {someone}\"\n";
    let catalog = format!("{}{body}", HEADER.replace("(n != 1)", "(n % 0)")).into_bytes();
    let folder = add(&scratch_dir("i18n"), "cats", Some(&catalog), "sv.po");
    let t = texts(&folder, "sv");
    assert!(t[3] == "2 cats" && t[5] == "Hi you", "{t:?}");
}

#[test]
fn the_region_falls_back_to_the_language_and_a_new_language_reloads() {
    let folder = add(&scratch_dir("i18n"), "cats", Some(&po(SV)), "pt.po");
    assert_eq!(texts(&folder, "pt_BR")[0], "Hej");
    assert_eq!(texts(&folder, "sv")[3], "2 cats", "no sv.po in the plugin");
    assert_eq!(telescope_core::i18n::tr("Hello"), "Hallå från Telescope");
    assert_eq!(texts(&folder, "pt_BR")[0], "Hej");
    assert_eq!(telescope_core::i18n::tr("Hello"), "Hello", "Telescope has no such text in this language");
}

#[test]
fn one_plugin_doesnt_see_anothers_texts() {
    let dir = scratch_dir("i18n");
    let cats = add(&dir, "cats", Some(&po(SV)), "sv.po");
    let dogs = add(&dir, "dogs", None, "");
    telescope_in("sv");
    // Both at once, as the window has them
    let (mut cats, mut dogs) = (PluginTest::open_in(&cats, "sv").unwrap(), PluginTest::open_in(&dogs, "sv").unwrap());
    let cat = |t: &mut PluginTest| t.call("texts", vec![]).unwrap()[3].clone();
    assert_eq!(cat(&mut cats), "2 katter");
    assert_eq!(cat(&mut dogs), "2 cats");
    assert_eq!(cat(&mut cats), "2 katter");
}

#[test]
fn everything_is_english_in_english() {
    let folder = add(&scratch_dir("i18n"), "cats", Some(&po(SV)), "sv.po");
    assert_eq!(texts(&folder, "en")[0], "Hello");
}

/// The command people run: its exit code, what it printed and what it complained about.
fn plugin_texts(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_plugin-texts")).args(args).output().unwrap();
    (out.status.code().unwrap(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn the_script_extracts_and_updates_a_plugins_own_catalogs() {
    let folder = add(&scratch_dir("i18n"), "cats", None, "");
    let path = folder.to_str().unwrap();
    let (code, _, said) = plugin_texts(&["new", "sv", path]);
    assert!(code == 1 && said.contains("run extract first"), "nothing extracted yet: {said}");
    assert_eq!(plugin_texts(&["extract", path]).0, 0);
    let template = telescope_plugin_host::i18n::read_po(&std::fs::read_to_string(folder.join("locale/cats.pot")).unwrap()).unwrap();
    let mut ids: Vec<&str> = template.iter().map(|e| e.msgid.as_str()).filter(|m| !m.is_empty()).collect();
    ids.sort();
    assert_eq!(ids, ["Camera", "Hello", "Hi {who}", "Not translated", "{n} cat"]);
    assert!(template.iter().filter(|e| !e.msgid.is_empty()).all(|e| e.locations[0].starts_with("cats/")));
    assert_eq!(plugin_texts(&["new", "sv", path]).0, 0);
    let po = folder.join("locale/sv.po");
    let text = std::fs::read_to_string(&po).unwrap().replace("msgid \"Hello\"\nmsgstr \"\"", "msgid \"Hello\"\nmsgstr \"Hej\"");
    std::fs::write(&po, text).unwrap();
    assert_eq!(plugin_texts(&["extract", path]).0, 0, "keeps the translation it was given");
    let (code, said, _) = plugin_texts(&["status", path]);
    assert!(code == 0 && said.contains("sv: 1/5 translated"), "{said}");
    assert_eq!(texts(&folder, "sv")[0], "Hej");
}

#[test]
fn the_script_wants_a_plugins_folder() {
    let dir = scratch_dir("i18n");
    add(&dir, "cats", None, "");
    let (code, _, said) = plugin_texts(&["extract", dir.to_str().unwrap()]);
    assert!(code != 0 && said.contains("point at your plugin's folder"), "{code} {said}");
    assert_eq!(plugin_texts(&["extract"]).0, 2, "no folder at all");
}
