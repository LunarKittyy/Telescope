"""The workbench renders production widgets and fails when its declared coverage changes."""

import copy
import json

import pytest

from scripts import translation_preview as preview
from telescope import i18n


QUICK = ["cool", "warm", "hot", "main-wide", "settings-general", "dialog-phones", "dialog-preset-save", "menu-presets",
         "notify-battery-low", "search-1"]
RECORD = json.loads((preview.ASSETS / "coverage.json").read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def scenes():
    """A few scenes of each kind; the whole set takes minutes, so `--check` renders it and this only samples."""
    return preview.render_all(QUICK)


def test_sampled_scenes_still_show_what_the_record_says(scenes):
    assert not [r["scene"] for r in scenes if sorted({x["key"] for x in r["regions"]}) != RECORD["scenes"][r["scene"]]]
    assert not RECORD["missing"] and RECORD["plural"]


@pytest.mark.skipif(not __import__("os").environ.get("TELESCOPE_FULL_PREVIEW"), reason="renders every scene, minutes of CPU")
def test_whole_record_matches_every_scene():
    actual = preview.coverage(preview.render_all())
    assert not preview.check_coverage(actual, RECORD)


def test_widget_adapters_are_exercised(scenes):
    assert any(r["widget"] == "Switch" for s in scenes for r in s["regions"])
    assert any(r["widget"] == "StatusPill" and r["values"] for s in scenes for r in s["regions"])


def test_disappearing_region_fails_even_when_another_scene_has_it(scenes):
    expected = preview.coverage(scenes)
    changed = copy.deepcopy(scenes)
    changed[0]["regions"].pop()
    assert preview.check_coverage(preview.coverage(changed), expected)


def test_new_text_is_reported_without_updating_a_manifest(scenes, monkeypatch):
    expected = preview.coverage(scenes)
    entries = preview.catalog()
    added = i18n.Entry("A new warning")
    added.locations = ["telescope/plugins/monitoring.py:999"]
    identity = preview.key("", added.msgid)
    entries[identity] = added
    monkeypatch.setattr(preview, "catalog", lambda: entries)
    actual = preview.coverage(scenes)
    assert identity in actual["missing"]
    assert preview.check_coverage(actual, expected)


def test_new_text_outside_the_scenes_also_fails(scenes, monkeypatch):
    expected = preview.coverage(scenes)
    entries = preview.catalog()
    added = i18n.Entry("A new camera feature")
    added.locations = ["telescope/plugins/camera_control.py:999"]
    entries[preview.key("", added.msgid)] = added
    monkeypatch.setattr(preview, "catalog", lambda: entries)
    assert preview.check_coverage(preview.coverage(scenes), expected)


def test_draft_changes_real_qt_image_and_keeps_exact_identity(scenes):
    identity = preview.key("", "Notify me")
    draft = "Preview test wording"
    result = preview.worker({"lang": "es", "scene": "cool", "drafts": {identity: draft}})
    uses = [r for r in result["regions"] if r["key"] == identity]
    assert len(uses) == 2 and all(r["shown"] == draft for r in uses)
    assert result["image"] != scenes[0]["image"]
    assert all(r["rect"][2] > 0 and r["rect"][3] > 0 for r in result["regions"])
    other = preview.key("", "Stop the stream")
    same = preview.worker({"lang": "es", "scene": "cool", "drafts": {identity: draft, other: draft}})
    assert {r["key"] for r in same["regions"] if r["shown"] == draft} == {identity, other}


@pytest.mark.parametrize("text", ["Too hot", "{wrong} degrees", "{temp", "{temp:invalid}"])
def test_bad_fields_are_rejected_before_render_or_export(text):
    identity = preview.key("", "The phone is at {temp} °C")
    with pytest.raises(ValueError):
        preview.validate({"drafts": {identity: text}})


def test_extreme_format_width_is_rejected_before_it_can_allocate_memory():
    with pytest.raises(ValueError, match='placeholder'):
        preview.validate({'drafts': {preview.key('', 'The phone is at {temp} °C'): 'Temperature {temp:1000000000}'}})


@pytest.mark.parametrize("payload", [{"lang": "../../other"}, {"scene": "unknown"},
                                     {"drafts": []}, {"drafts": {"unknown": "text"}}, []])
def test_invalid_input_is_rejected(payload):
    with pytest.raises(ValueError):
        preview.validate(payload)


def test_editor_from_an_older_checkout_is_rejected():
    with pytest.raises(ValueError, match="checkout changed"):
        preview.validate({"revision": "an older revision"})


def test_other_websites_cannot_submit_preview_requests():
    from types import SimpleNamespace
    handler = object.__new__(preview.Handler)
    handler.server = SimpleNamespace(server_port=8769)
    handler.headers = {"Host": "127.0.0.1:8769", "Origin": "https://another.example"}
    assert not handler.allowed()
    handler.headers["Origin"] = "http://127.0.0.1:8769"
    assert handler.allowed()
    handler.headers["Host"] = "another.example:8769"
    assert not handler.allowed()


def test_export_round_trips_without_touching_catalog(tmp_path):
    path = i18n.LOCALE_DIR / "es.po"
    before = path.read_bytes()
    identity = preview.key("", "Notify me")
    result = preview.export_po({"lang": "es", "drafts": {identity: "Draft wording"}})
    exported = tmp_path / "es.po"
    exported.write_bytes(result)
    entries, _ = i18n.read_po(exported)
    assert next(e.msgstr for e in entries if e.key == ("", "Notify me")) == ["Draft wording"]
    assert path.read_bytes() == before


def test_placeholder_preview_and_long_word_fit_warning():
    identity = preview.key("", "The phone is at {temp} °C")
    result = preview.worker({"lang": "es", "scene": "hot", "drafts": {identity: "Very hot: {temp} " + "W" * 300}})
    use = next(r for r in result["regions"] if r["key"] == identity)
    assert "{temp}" not in use["shown"] and use["values"]["temp"] in use["shown"]
    assert result["findings"]


def test_language_progress_ignores_fuzzy_missing_partial_and_obsolete_entries():
    current = [i18n.Entry(text) for text in ('Done', 'Fuzzy', 'Empty', 'Missing', 'Plural')]
    current[-1].plural = 'Plurals'
    found = copy.deepcopy(current)
    found[0].msgstr = ['Finished']
    found[1].msgstr = ['Needs checking']
    found[1].flags = ['fuzzy']
    found[2].msgstr = ['']
    found.pop(3)
    found[-1].msgstr = ['One', '']
    obsolete = i18n.Entry('Removed')
    obsolete.msgstr = ['Old translation']
    entries = {preview.key(e.ctx, e.msgid): e for e in current}
    assert preview.language_progress(entries, [*found, obsolete]) == {'translated': 1, 'total': 5}
    found[-1].msgstr = ['One', 'Many']
    assert preview.language_progress(entries, found) == {'translated': 2, 'total': 5}


def test_every_scene_belongs_to_a_section_and_the_sample_renders_something(scenes):
    listed = [scene for section in preview.scenes.sections() for scene in section["scenes"]]
    assert sorted(listed) == sorted(preview.SCENES)
    assert all(result["regions"] for result in scenes)
    assert all(r["rect"][2] > 0 and r["rect"][3] > 0 for result in scenes for r in result["regions"])


def test_settings_scenes_match_the_pages_the_window_has():
    import subprocess
    import sys
    code = ("import contextlib, json, os\n"
            "from PyQt6.QtWidgets import QApplication\n"
            "app = QApplication([])\n"
            "from scripts import translation_scenes as m\n"
            "with contextlib.ExitStack() as stack:\n"
            "    m.use_scratch_profile(stack)\n"
            "    window = m.main_window(stack)\n"
            "    window.open_settings()\n"
            "    print(json.dumps(list(window._settings_win.tab_names())), flush=True)\n"
            "    os._exit(0)\n")
    env = {**__import__("os").environ, "QT_QPA_PLATFORM": "offscreen"}
    out = subprocess.run([sys.executable, "-c", code], cwd=preview.ROOT, env=env, capture_output=True, text=True, timeout=60)
    assert json.loads(out.stdout.splitlines()[-1]) == [tab for tab, _title in preview.scenes.SETTINGS_PAGES], out.stderr


def test_menus_dialogs_and_lists_are_mapped_to_their_own_items(scenes):
    by_id = {result["scene"]: result for result in scenes}
    assert any(r["widget"] == "QMenu" for r in by_id["menu-presets"]["regions"])
    assert any(r["slot"] == "window title" for r in by_id["dialog-phones"]["regions"])
    assert any(r["slot"] == "label" for r in by_id["dialog-preset-save"]["regions"])
    assert any(r["widget"] == "QListWidget" for r in by_id["settings-general"]["regions"])


def test_every_text_has_a_scene_except_plurals():
    entries = preview.catalog()
    assert all(entries[k].plural for k in RECORD["plural"])
    shown = {k for keys in RECORD["scenes"].values() for k in keys}
    assert set(entries) - shown == set(RECORD["plural"]) - shown
    assert RECORD["screens"] and len(RECORD["screens"]) > len(RECORD["messages_only"])
