"""Translations: the catalogs stay in step with the code, a translation can't break the window, and tr() is never
called before a language is loaded."""

import ast
from pathlib import Path

import pytest

from telescope import i18n

ROOT = Path(__file__).resolve().parent.parent
FIX = "run `python scripts/i18n.py extract` in desktop/ and commit telescope/locale"


@pytest.fixture
def catalogs(tmp_path, monkeypatch):
    """A locale folder of the test's own, and English again afterwards."""
    monkeypatch.setattr(i18n, "LOCALE_DIR", tmp_path)
    monkeypatch.delenv(i18n.ENV_LANGUAGE, raising=False)
    yield tmp_path
    i18n.load("en")


def _po(folder: Path, lang: str, body: str, plural: str = "nplurals=2; plural=(n != 1);"):
    header = (f'msgid ""\nmsgstr ""\n"Language: {lang}\\n"\n"Content-Type: text/plain; charset=UTF-8\\n"\n'
              f'"Plural-Forms: {plural}\\n"\n\n')
    (folder / f"{lang}.po").write_text(header + body, encoding="utf-8")


# ── The catalogs and the code ────────────────────────────────────────────────

def test_every_text_can_be_found():
    _entries, problems = i18n.find_all(ROOT)
    assert not problems, "\n".join(problems)


def test_the_template_has_every_text_in_the_code():
    found, _ = i18n.find_all(ROOT)
    template, _ = i18n.read_po(i18n.LOCALE_DIR / "telescope.pot")
    want = {(e.ctx, e.msgid, e.plural) for e in found}
    have = {(e.ctx, e.msgid, e.plural) for e in template}
    assert want == have, f"telescope.pot is out of date: {FIX}"


@pytest.mark.parametrize("po", sorted(i18n.LOCALE_DIR.glob("*.po")), ids=lambda p: p.stem)
def test_each_language_has_the_template_and_working_fields(po):
    entries, header = i18n.read_po(po)
    template, _ = i18n.read_po(i18n.LOCALE_DIR / "telescope.pot")
    assert {e.key for e in entries} == {e.key for e in template}, f"{po.name} is out of date: {FIX}"
    count = i18n.plural_count(header)
    assert i18n.plural_rule(header)(1) < count
    for e in entries:
        want = i18n.fields(e.msgid) | i18n.fields(e.plural)
        for text in e.msgstr:
            assert i18n.fields(text) <= want, f"{po.name}: {e.msgid!r} has a {{field}} the English doesn't"



def _runs_at_import(node: ast.AST):
    """What runs when the module is imported: everything but function bodies (class bodies and default arguments
    do run)."""
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
        args = node.args
        for part in args.defaults + [d for d in args.kw_defaults if d is not None] + getattr(node, "decorator_list", []):
            yield from _runs_at_import(part)
        return
    yield node
    for child in ast.iter_child_nodes(node):
        yield from _runs_at_import(child)


@pytest.mark.parametrize("path", sorted((ROOT / "telescope").rglob("*.py")), ids=lambda p: p.stem)
def test_nothing_is_translated_at_import_time(path):
    tree = ast.parse(path.read_text(encoding="utf-8"))
    early = sorted({n.lineno for n in _runs_at_import(tree)
                    if isinstance(n, ast.Call) and isinstance(n.func, ast.Name) and n.func.id in ("tr", "trn")})
    assert not early, f"{path.name} lines {early}: the language isn't loaded yet there; mark the text with N_()"


# ── Looking texts up ─────────────────────────────────────────────────────────

def test_english_is_what_the_code_says():
    i18n.load("en")
    assert i18n.tr("Paired with {name}", name="Pixel") == "Paired with Pixel"
    assert i18n.trn("{n} phone", "{n} phones", 1) == "1 phone"
    assert i18n.trn("{n} phone", "{n} phones", 3) == "3 phones"


def test_a_translation_is_used_with_its_fields_and_plural_forms(catalogs):
    _po(catalogs, "sv", '''msgid "Paired with {name}"
msgstr "Parkopplad med {name}"

msgid "{n} phone"
msgid_plural "{n} phones"
msgstr[0] "{n} telefon"
msgstr[1] "{n} telefoner"

msgctxt "search"
msgid "audio sound"
msgstr "ljud"
''')
    assert i18n.load("sv") == "sv"
    assert i18n.tr("Paired with {name}", name="Pixel") == "Parkopplad med Pixel"
    assert i18n.trn("{n} phone", "{n} phones", 1) == "1 telefon"
    assert i18n.trn("{n} phone", "{n} phones", 2) == "2 telefoner"
    assert i18n.tr("audio sound", ctx="search") == "ljud"
    assert i18n.tr("audio sound") == "audio sound"  # another context
    assert i18n.tr("Not translated yet") == "Not translated yet"


def test_a_broken_or_unfinished_translation_shows_the_english(catalogs):
    _po(catalogs, "sv", '''msgid "Paired with {name}"
msgstr "Parkopplad med {namn}"

#, fuzzy
msgid "Remove"
msgstr "Ta bort"

msgid "Cancel"
msgstr ""
''')
    i18n.load("sv")
    assert i18n.tr("Paired with {name}", name="Pixel") == "Paired with Pixel"
    assert i18n.tr("Remove") == "Remove"
    assert i18n.tr("Cancel") == "Cancel"


def test_a_language_with_three_plural_forms(catalogs):
    rule = "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);"
    _po(catalogs, "pl", '''msgid "{n} phone"
msgid_plural "{n} phones"
msgstr[0] "{n} telefon"
msgstr[1] "{n} telefony"
msgstr[2] "{n} telefonów"
''', plural=rule)
    i18n.load("pl")
    assert [i18n.trn("{n} phone", "{n} phones", n) for n in (1, 3, 5, 22)] == [
        "1 telefon", "3 telefony", "5 telefonów", "22 telefony"]


def test_a_text_keeps_its_english_when_another_is_translated_alike(catalogs):
    # What a fold is saved under: "Bild" alone can't say whether it was Picture or Image
    _po(catalogs, "sv", 'msgid "Picture"\nmsgstr "Bild"\n\nmsgid "Image"\nmsgstr "Bild"\n')
    i18n.load("sv")
    assert i18n.english(i18n.tr("Image")) == "Image"
    assert i18n.english(i18n.tr("Picture")) == "Picture"
    assert i18n.english("Ljus") == "Ljus"  # text that didn't come from tr() is looked up, and is itself if unknown


def test_a_list_reads_naturally():
    i18n.load("en")
    assert i18n.and_list(["Camera"]) == "Camera"
    assert i18n.and_list(["Camera", "Microphone", "Preview"]) == "Camera, Microphone and Preview"


def test_shown_numbers_follow_the_language(catalogs):
    from PyQt6.QtCore import QLocale
    i18n.load("en")
    assert i18n.number(1, 1) == "1.0"
    assert i18n.number(-0.5, 1, signed=True) == "-0.5"
    assert i18n.number(10, 0, signed=True) == "+10"
    assert i18n.percent(80) == "80%"
    assert i18n.percent(-10, signed=True) == "-10%"
    assert QLocale().decimalPoint() == "."
    _po(catalogs, "sv", 'msgctxt "percentage"\nmsgid "{n}%"\nmsgstr "{n} %"\n')
    assert i18n.load("sv") == "sv"
    assert i18n.number(1, 1) == "1,0"
    assert i18n.number(45.5, 1) == "45,5"
    assert i18n.number(10, 0, signed=True) == "+10"
    assert i18n.percent(80) == "80 %"
    assert i18n.percent(12.5, 1) == "12,5 %"
    assert QLocale().decimalPoint() == ","  # spinboxes follow too
    i18n.load("en")
    assert QLocale().decimalPoint() == "."


def test_the_language_follows_the_system_unless_set(catalogs, monkeypatch):
    _po(catalogs, "sv", "")
    _po(catalogs, "pt_BR", "")
    assert i18n.available() == ["pt_BR", "sv"]
    assert i18n.pick(["sv-SE", "en-US"], i18n.available()) == "sv"
    assert i18n.pick(["pt-BR"], i18n.available()) == "pt_BR"
    assert i18n.pick(["de-DE", "en-GB", "sv"], i18n.available()) == "en"  # English comes before Swedish there
    assert i18n.pick(["fr"], i18n.available()) == "en"
    monkeypatch.setenv(i18n.ENV_LANGUAGE, "sv")
    assert i18n.load() == "sv"


def test_the_language_picked_in_settings_wins_over_the_system(catalogs, monkeypatch):
    _po(catalogs, "sv", "")
    monkeypatch.setattr(i18n, "system_languages", lambda: ["sv-SE"])
    assert i18n.resolve("") == "sv"
    assert i18n.resolve("en") == "en"
    assert i18n.resolve("de") == "en"  # a language that's gone since it was picked
    monkeypatch.setenv(i18n.ENV_LANGUAGE, "en")
    assert i18n.resolve("sv") == "en"
    assert i18n.native_name("sv") == "Svenska"
    assert i18n.native_name("en") == "English"


def test_a_first_draft_says_so(catalogs):
    _po(catalogs, "sv", "")
    _po(catalogs, "es", "", plural="nplurals=2; plural=(n != 1);\\n\"\n\"X-Telescope-Draft: yes")
    assert i18n.is_draft("es")
    assert not i18n.is_draft("sv")
    assert not i18n.is_draft("en")
    assert not i18n.is_draft("de")  # no catalog at all


def test_an_unreadable_catalog_leaves_english(catalogs):
    (catalogs / "sv.po").write_text('msgid "x"\nnonsense\n', encoding="utf-8")
    assert i18n.load("sv") == "en"


def test_po_files_keep_what_they_hold(tmp_path):
    e = i18n.Entry('Say "hi"\n\nthen\tgo', ctx="search")
    e.msgstr = ['Säg "hej"\n\nsen\tgå']
    e.comments, e.locations, e.flags = ["a note"], ["telescope/app.py:1"], ["fuzzy"]
    p = i18n.Entry("{n} phone", plural="{n} phones")
    p.msgstr = ["{n} telefon", "{n} telefoner"]
    path = tmp_path / "x.po"
    i18n.write_po(path, "Language: sv\n", [e, p])
    back, header = i18n.read_po(path)
    assert header == "Language: sv\n"
    assert [(b.ctx, b.msgid, b.plural, b.msgstr, b.comments, b.flags) for b in back] == [
        ("search", e.msgid, "", e.msgstr, ["a note"], ["fuzzy"]),
        ("", "{n} phone", "{n} phones", p.msgstr, [], [])]


def test_a_field_can_have_any_name():
    i18n.load("en")
    assert i18n.tr("Nothing matches {text}", text="zz") == "Nothing matches zz"
    assert i18n.trn("{n} of {singular}", "{n} of {singular}", 2, singular="x") == "2 of x"
