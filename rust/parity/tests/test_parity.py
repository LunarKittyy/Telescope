"""The parity tools' own logic, without the Python app or a Rust binary: python -m pytest -q rust/parity/tests"""

import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import compare_config  # noqa: E402
import inventory  # noqa: E402
from inventory import Item  # noqa: E402

EXPECTED = {
    "set_aside": False,
    "plugins": ["camera_control", "setup"],
    "saved": {
        "version": 3,
        "plugin_configs": {"setup": {"language": "sv", "theme": "dark"}, "hologram": {"glow": 1}},
        "devices": {"phone-1": {"plugin_configs": {"camera_control": {"iso": 100, "ois": True}}}},
    },
}


def changed(path, value):
    import copy
    data = copy.deepcopy(EXPECTED)
    *parents, last = path
    target = data
    for part in parents:
        target = target[part]
    target[last] = value
    return data


def test_only_ported_settings_are_compared():
    done = {("global", "setup", "language")}
    builtin = {"setup", "camera_control"}
    assert compare_config.compare(EXPECTED, changed(["saved", "plugin_configs", "setup", "theme"], "light"), done, builtin) == []
    diffs = compare_config.compare(EXPECTED, changed(["saved", "plugin_configs", "setup", "language"], "de"), done, builtin)
    assert diffs == ['saved.plugin_configs.setup.language: expected "sv", got "de"']


def test_device_settings_use_the_device_scope():
    builtin = {"setup", "camera_control"}
    other = changed(["saved", "devices", "phone-1", "plugin_configs", "camera_control", "iso"], 200)
    assert compare_config.compare(EXPECTED, other, {("global", "camera_control", "iso")}, builtin) == []
    assert compare_config.compare(EXPECTED, other, {("device", "camera_control", "iso")}, builtin) == [
        "saved.devices.phone-1.plugin_configs.camera_control.iso: expected 100, got 200"]


def test_config_level_parts_are_always_compared():
    builtin = {"setup", "camera_control"}
    assert compare_config.compare(EXPECTED, changed(["set_aside"], True), set(), builtin)
    assert compare_config.compare(EXPECTED, changed(["saved", "version"], 4), set(), builtin)
    assert compare_config.compare(EXPECTED, changed(["saved", "plugin_configs", "hologram"], {}), set(), builtin)
    assert compare_config.compare(EXPECTED, changed(["saved", "devices"], {}), set(), builtin, original={"phone-1"})


def test_device_entries_count_only_where_config_py_or_a_ported_plugin_decides():
    builtin = {"setup", "camera_control", "browser_camera"}
    browser = changed(["saved", "devices", "browser:aaa"], {"plugin_configs": {}})
    # Browser camera forgetting a browser is its own behaviour, until all of its settings are ported
    assert compare_config.compare(browser, EXPECTED, set(), builtin, original={"phone-1", "browser:aaa"}) == []
    assert compare_config.compare(browser, EXPECTED, set(), builtin, original={"phone-1", "browser:aaa"},
                                  whole={"browser_camera"}) == ["saved.devices.browser:aaa (kept): expected true, got false"]
    # A 2.x phone that config.py drops
    kept = changed(["saved", "devices", "Pixel 8"], {})
    assert compare_config.compare(EXPECTED, kept, set(), builtin, original={"Pixel 8"}) == [
        "saved.devices.Pixel 8 (kept): expected false, got true"]
    # An entry the app made itself counts through its ported settings
    assert compare_config.compare(EXPECTED, changed(["saved", "devices"], {}), set(), builtin) == []
    assert compare_config.compare(EXPECTED, changed(["saved", "devices"], {}), {("device", "camera_control", "iso")},
                                  builtin) == ["saved.devices.phone-1.plugin_configs.camera_control.iso: expected 100, got (missing)"]


def test_a_ported_entry_must_point_at_something_that_exists():
    assert inventory.problems_with({"status": "ported", "rust": "rust/parity/inventory.py::check"}) == []
    assert inventory.problems_with({"status": "verified", "rust": "rust/parity/inventory.py:12"}) == []
    assert inventory.problems_with({"status": "ported", "rust": "rust/core/src/session.rs"}) == [
        "rust = 'rust/core/src/session.rs' doesn't exist (moved?)"]


def test_a_missing_key_is_a_difference():
    other = changed(["saved", "plugin_configs", "setup"], {"theme": "dark"})
    assert compare_config.compare(EXPECTED, other, {("global", "setup", "language")}, {"setup"}) == [
        'saved.plugin_configs.setup.language: expected "sv", got (missing)']


ITEMS = [Item("text", "Start", "connection"), Item("text", "ctx|Stop", "connection"), Item("test", "test_a.py::test_b", "a")]


def test_update_adds_new_items_and_keeps_what_people_wrote(tmp_path):
    ledger = {"text": {"Start": {"status": "ported", "rust": "rust/parity"}, "Gone": {"status": "todo"},
                       "Renamed": {"status": "dropped", "reason": "merged"}}}
    added, removed, kept = inventory.update(ITEMS, ledger)
    assert (added, removed, kept) == (2, 1, ["text:Renamed"])
    assert ledger["text"]["Start"] == {"status": "ported", "rust": "rust/parity"}
    assert ledger["text"]["ctx|Stop"] == {"status": "todo"}


def test_the_ledger_survives_a_round_trip(tmp_path):
    path = tmp_path / "ledger.toml"
    ledger = {"unknown": {"tray-theme": {"status": "todo", "note": 'says "dark"'}},
              "text": {"Start": {"status": "ported", "rust": "rust/parity", "note": "a\nb"}, "ctx|Stop": {"status": "todo"}},
              "test": {"test_a.py::test_b": {"status": "dropped", "reason": "Qt only"}}}
    inventory.write_ledger(ledger, ITEMS, path)
    with path.open("rb") as f:
        assert tomllib.load(f) == {**ledger, **{k: {} for k in inventory.KINDS if k not in ledger}}
    assert inventory.check(ITEMS, inventory.read_ledger(path)) == []


def test_check_finds_missing_gone_and_bad_entries():
    ledger = {"text": {"Start": {"status": "ported"}, "Gone": {"status": "todo"}},
              "test": {"test_a.py::test_b": {"status": "done"}}, "unknown": {"x": {"status": "todo"}}}
    assert inventory.check(ITEMS, ledger) == [
        "not in the ledger: text:ctx|Stop",
        "gone from the Python app but still in the ledger: text:Gone",
        "text:Start: ported needs rust = where it lives",
        "test:test_a.py::test_b: status 'done' isn't one of todo, ported, verified, dropped",
        "unknown:x: needs a note saying what it is",
    ]


def test_report_counts_done_per_area():
    ledger = {"text": {"Start": {"status": "verified", "rust": "rust/parity"}, "ctx|Stop": {"status": "todo"}},
              "test": {"test_a.py::test_b": {"status": "dropped", "reason": "x"}}}
    text = inventory.report(ITEMS, ledger)
    assert "connection  1 of 2 texts" in text
    assert "a           1 of 1 tests" in text
    assert "1 of     1  with every test done" in text


def test_tests_go_to_the_longest_area_they_start_with():
    areas = {"shortcuts", "platform", "connection", "connection_dialogs"}
    assert inventory.test_area("test_shortcuts_plugin.py", areas) == "shortcuts"
    assert inventory.test_area("test_platform_linux.py", areas) == "platform"
    assert inventory.test_area("test_connection_dialogs.py", areas) == "connection_dialogs"
    assert inventory.test_area("test_main.py", areas) == "main"


def recording(tmp_path, name, lines):
    import json
    path = tmp_path / name
    path.write_text("\n".join(json.dumps(line) for line in lines) + "\n", encoding="utf-8")
    return path


def control(action, value, t=1.0):
    return {"t": t, "port": 8080, "method": "POST", "path": "/v1/control", "query": {},
            "body": {"action": action, "value": value}}


def poll(path, port=8766):
    return {"t": 1.0, "port": port, "method": "GET", "path": path, "query": {}, "body": None}


def test_phone_recordings_match_when_actions_only_interleave_differently(tmp_path):
    import compare_phone
    a = recording(tmp_path, "a", [{"mark": "stream"}, control("ae_comp", 0), control("ois", "1"), control("ae_comp", 3),
                                  poll("/v1/ping"), poll("/v1/ping")])
    b = recording(tmp_path, "b", [{"mark": "stream"}, control("ois", "1"), control("ae_comp", 0), poll("/v1/ping"),
                                  control("ae_comp", 3)])
    assert compare_phone.compare(compare_phone.phases(a), compare_phone.phases(b)) == []
    assert compare_phone.compare(compare_phone.phases(a), compare_phone.phases(b), strict=True)


def test_phone_recordings_differ_on_values_order_and_requests(tmp_path):
    import compare_phone
    a = compare_phone.phases(recording(tmp_path, "a", [{"mark": "s"}, control("ae_comp", 0), control("ae_comp", 3),
                                                       poll("/v1/state", 8080)]))
    b = compare_phone.phases(recording(tmp_path, "b", [{"mark": "s"}, control("ae_comp", 3), control("ae_comp", 0)]))
    lines = compare_phone.compare(a, b, names=("py", "rs"))
    assert lines[0] == "[s] differs"
    assert "  only py asks: GET :8080 /v1/state" in lines
    assert compare_phone.compare(a, b, unordered=True, names=("py", "rs")) == [
        "[s] differs", "  only py asks: GET :8080 /v1/state"]
