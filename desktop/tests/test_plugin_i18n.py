"""A plugin someone added can ship its own translations: locale/<lang>.po in its folder, used for its texts before
Telescope's own, with English for what's missing, and a file that's broken costs it nothing but the translation."""

import json
import sys

import pytest

from telescope import i18n, plugin_loader

_HEADER = ('msgid ""\nmsgstr ""\n"Content-Type: text/plain; charset=UTF-8\\n"\n'
           '"Plural-Forms: nplurals=2; plural=(n != 1);\\n"\n\n')
_CODE = '''from telescope.i18n import N_, tr, trn
from telescope.plugin import TelescopePlugin
from . import helper

LATER = N_("Hello")


class P(TelescopePlugin):
    name = {name!r}

    def texts(self):
        return [tr("Hello"), tr("Not translated"), tr("Camera"), trn("{{n}} cat", "{{n}} cats", 2), tr(LATER),
                tr("Hi {{who}}", who="you"), helper.hello()]
'''
_HELPER = "from telescope.i18n import tr\n\n\ndef hello():\n    return tr('Hello')\n"
_SV = ('msgid "Hello"\nmsgstr "Hej"\n\nmsgid "{n} cat"\nmsgid_plural "{n} cats"\nmsgstr[0] "{n} katt"\n'
       'msgstr[1] "{n} katter"\n\nmsgid "Hi {who}"\nmsgstr "Hej {who}"\n')


@pytest.fixture
def world(config_home, tmp_path, monkeypatch):
    """A plugins folder, Telescope's own Swedish catalog (it knows Camera) and a clean slate for plugin catalogs."""
    plugins = tmp_path / "plugins"
    plugins.mkdir(mode=0o755)
    monkeypatch.setattr(plugin_loader, "plugins_dir", lambda: plugins)
    own = tmp_path / "locale"
    own.mkdir()
    (own / "sv.po").write_text(_HEADER + 'msgid "Camera"\nmsgstr "Kamera"\n\nmsgid "Hello"\nmsgstr "Hallå från Telescope"\n',
                               encoding="utf-8")
    monkeypatch.setattr(i18n, "LOCALE_DIR", own)
    monkeypatch.setattr(i18n, "_plugin_folders", {})
    monkeypatch.setattr(i18n, "_plugin_catalogs", {})
    monkeypatch.delenv(i18n.ENV_LANGUAGE, raising=False)
    yield plugins
    i18n.load("en")
    for name in [m for m in sys.modules if m.startswith("telescope_ext")]:
        del sys.modules[name]


def _add(folder, name, catalog=None, filename="sv.po"):
    target = folder / name
    target.mkdir()
    (target / "plugin.json").write_text(json.dumps({"name": name, "api": 1, "class": "P"}))
    (target / "__init__.py").write_text(_CODE.format(name=name))
    (target / "helper.py").write_text(_HELPER)
    if catalog is not None:
        (target / "locale").mkdir()
        body = catalog if isinstance(catalog, bytes) else (_HEADER + catalog).encode("utf-8")
        (target / "locale" / filename).write_bytes(body)
    return target


def _texts(folder) -> list:
    (found,) = [p for p in plugin_loader.find_installed(folder=folder)]
    plugin_loader.set_enabled(found, True)
    loaded, failed = plugin_loader.load_enabled(folder=folder)
    assert failed == [], [f.error for f in failed]
    return loaded[0].texts()


def test_a_plugin_with_a_catalog_shows_its_texts_in_the_language(world):
    _add(world, "cats", _SV)
    i18n.load("sv")
    texts = _texts(world)
    assert texts[0] == "Hej"  # the plugin's catalog beats Telescope's translation of the same English
    assert texts[3] == "2 katter"
    assert texts[4] == "Hej"  # a text marked with N_() at import time, translated where it's shown
    assert texts[5] == "Hej you"
    assert texts[6] == "Hej"  # from another module of the plugin
    assert i18n.tr("Hello") == "Hallå från Telescope"  # Telescope's own code still uses its own catalog


def test_a_plugin_without_a_catalog_stays_in_english_but_gets_telescopes_words(world):
    _add(world, "cats")
    i18n.load("sv")
    texts = _texts(world)
    assert texts[0] == "Hallå från Telescope"  # Telescope's own translation of the same English
    assert texts[1] == "Not translated" and texts[3] == "2 cats"


def test_a_plugin_with_another_language_only_has_english(world):
    _add(world, "cats", _SV, filename="de.po")
    i18n.load("sv")
    assert _texts(world)[3] == "2 cats"


def test_a_text_the_plugin_hasnt_translated_falls_back_to_telescope_then_english(world):
    _add(world, "cats", _SV)
    i18n.load("sv")
    texts = _texts(world)
    assert texts[1] == "Not translated"  # nobody has it: English
    assert texts[2] == "Kamera"  # only Telescope has it


def test_an_empty_or_fuzzy_translation_counts_as_missing(world):
    _add(world, "cats", 'msgid "Hello"\nmsgstr ""\n\n#, fuzzy\nmsgid "Not translated"\nmsgstr "Gammal"\n')
    i18n.load("sv")
    texts = _texts(world)
    assert texts[0] == "Hallå från Telescope" and texts[1] == "Not translated"


@pytest.mark.parametrize("broken", [
    'msgid "Hello"\nnonsense\n',
    'msgid "Hello"\nmsgstr "Hej"\n"unfinished\n',
    'msgid "{n} cat"\nmsgid_plural "{n} cats"\nmsgstr[x] "a"\n',
    b"\xff\xfe\x00 not text",
])
def test_a_broken_file_leaves_the_plugin_in_english_and_still_loading(world, broken):
    _add(world, "cats", broken)
    i18n.load("sv")
    texts = _texts(world)  # it loaded
    assert texts[3] == "2 cats" and texts[5] == "Hi you"
    assert texts[0] == "Hallå från Telescope"


def test_a_broken_plural_rule_or_field_shows_english_instead_of_failing(world):
    body = ('msgid "{n} cat"\nmsgid_plural "{n} cats"\nmsgstr[0] "a"\nmsgstr[1] "b"\n\n'
            'msgid "Hi {who}"\nmsgstr "Hej {someone}"\n')
    target = _add(world, "cats", body)
    (target / "locale" / "sv.po").write_text(
        _HEADER.replace("(n != 1)", "(n % 0)") + body, encoding="utf-8")
    i18n.load("sv")
    texts = _texts(world)
    assert texts[3] == "2 cats" and texts[5] == "Hi you"


def test_the_region_falls_back_to_the_language_and_a_new_language_reloads(world):
    _add(world, "cats", _SV, filename="pt.po")
    (i18n.LOCALE_DIR / "pt_BR.po").write_text(_HEADER + 'msgid "Camera"\nmsgstr "Câmera"\n', encoding="utf-8")
    i18n.load("pt_BR")
    assert _texts(world)[0] == "Hej"
    i18n.load("sv")  # no sv.po in the plugin
    (found,) = plugin_loader.find_installed(folder=world)
    assert plugin_loader.load(found).texts()[3] == "2 cats"
    i18n.load("pt_BR")
    assert i18n.tr("Hello") == "Hello"
    assert plugin_loader.load(found).texts()[0] == "Hej"


def test_one_plugin_doesnt_see_anothers_texts(world):
    _add(world, "cats", _SV)
    _add(world, "dogs")
    i18n.load("sv")
    for found in plugin_loader.find_installed(folder=world):
        plugin_loader.set_enabled(found, True)
    loaded, failed = plugin_loader.load_enabled(folder=world)
    assert failed == []
    by_name = {p.name: p.texts() for p in loaded}
    assert by_name["cats"][3] == "2 katter" and by_name["dogs"][3] == "2 cats"


def test_everything_is_english_in_english(world):
    _add(world, "cats", _SV)
    i18n.load("en")
    assert _texts(world)[0] == "Hello"


def _script():
    import importlib.util
    from pathlib import Path
    spec = importlib.util.spec_from_file_location("i18n_script", Path(__file__).parent.parent / "scripts" / "i18n.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_script_extracts_and_updates_a_plugins_own_catalogs(world, capsys):
    script = _script()
    folder = _add(world, "cats")
    assert script.main(["new", "sv", "--plugin", str(folder)]) == 1  # nothing extracted yet
    assert script.main(["extract", "--plugin", str(folder)]) == 0
    template, _ = i18n.read_po(folder / "locale" / "cats.pot")
    assert {e.msgid for e in template} == {"Hello", "Not translated", "Camera", "{n} cat", "Hi {who}"}
    assert all(e.locations[0].startswith("cats/") for e in template)
    assert script.main(["new", "sv", "--plugin", str(folder)]) == 0
    po = folder / "locale" / "sv.po"
    text = po.read_text(encoding="utf-8").replace('msgid "Hello"\nmsgstr ""', 'msgid "Hello"\nmsgstr "Hej"')
    po.write_text(text, encoding="utf-8")
    assert script.main(["extract", "--plugin", str(folder)]) == 0  # keeps the translation it was given
    assert script.main(["status", "--plugin", str(folder)]) == 0
    assert "sv: 1/5 translated" in capsys.readouterr().out
    i18n.load("sv")
    assert _texts(world)[0] == "Hej"


def test_the_script_wants_a_plugins_folder(world):
    with pytest.raises(SystemExit):
        _script().main(["extract", "--plugin", str(world)])
