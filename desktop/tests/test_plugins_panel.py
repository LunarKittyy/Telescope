"""Switching plugins on and off: what the window loads, what it says, and the Plugins tab."""

import ast
import json
import sys

import pytest
from PyQt6.QtTest import QTest
from PyQt6.QtWidgets import QCheckBox, QLabel

import telescope.widgets.plugins_panel as dialog_module
from telescope import plugin_loader
from telescope.plugin import SettingsSection
from telescope.widgets.plugins_panel import PluginsPanel
from telescope.widgets.settings_window import SettingsWindow
from test_app import _Plugin, window  # noqa: F401 (the fixture)


class _Builtin(_Plugin):
    def __init__(self, name, title, required=False):
        super().__init__(name)
        self.title, self.about, self.required = title, f"What {title} does.", required
        self.when_off = f"The {title} card goes away."
        self.selected_device = None  # as the window asks Connection


def _builtins():
    return [_Builtin("connection", "Connection", required=True), _Builtin("camera_control", "Camera"),
            _Builtin("presets", "Presets")]


@pytest.fixture
def plugins(config_home, tmp_path, monkeypatch):
    folder = tmp_path / "plugins"
    folder.mkdir(mode=0o755)
    monkeypatch.setattr(plugin_loader, "plugins_dir", lambda: folder)
    yield folder
    for name in [m for m in sys.modules if m.startswith("telescope_ext.")]:
        del sys.modules[name]


def _added(folder, name, header="return None", **manifest):
    target = folder / name
    target.mkdir()
    (target / "plugin.json").write_text(json.dumps({"name": name, "title": name.title(), "api": 1,
                                                    "class": "P", **manifest}))
    (target / "__init__.py").write_text(
        "from telescope.plugin import TelescopePlugin\nfrom PyQt6.QtWidgets import QLabel\n\n"
        f"class P(TelescopePlugin):\n    name = {name!r}\n\n    def create_panel(self):\n"
        "        return QLabel('card')\n\n"
        f"    def create_header_widget(self):\n        {header}\n")
    return target


def test_a_built_in_plugin_switched_off_isnt_loaded_and_the_window_says_so_once(window, plugins):
    plugin_loader.set_builtin_off("presets", True)
    plugin_loader.set_builtin_off("connection", True)  # required: loaded anyway
    window.load_plugins(_builtins())
    assert [p.name for p in window._plugins] == ["connection", "camera_control"]
    assert [p.name for p in window.plugins_off()] == ["presets"]
    issue = window._banners.issue("plugins_off")
    assert issue.title == "Presets is turned off" and issue.actions[0].label == "Plugins…"
    issue.on_dismiss()
    window._banners.clear_issue("plugins_off")
    window._remind_plugins_off()
    assert window._banners.issue("plugins_off") is None  # seen: not again until another one goes off
    assert "Plugins off: presets" in window.diagnostics_report()


def test_the_plugins_tab_says_how_many_are_off(window, plugins):
    plugin_loader.set_builtin_off("presets", True)
    plugin_loader.set_builtin_off("camera_control", True)
    window.load_plugins(_builtins())
    window.open_settings()
    try:
        assert window._settings_win.tab_names()["plugins"] == "Plugins (2 off)"
    finally:
        window._settings_win.close()


def test_an_added_plugin_switched_on_loads(window, plugins):
    _added(plugins, "extra")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    window.load_plugins(_builtins())
    assert "extra" in {p.name for p in window._plugins}
    assert window._banners.issue("plugins") is None
    assert "Added plugins: extra (running)" in window.diagnostics_report()


def test_an_added_plugin_that_breaks_while_starting_is_taken_back_out(window, plugins):
    _added(plugins, "broken", header="raise RuntimeError('no card today')")  # its card is in by then
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    window.load_plugins(_builtins())
    assert "broken" not in {p.name for p in window._plugins} and window._plugin("broken") is None
    cards = [w for widgets in window._panels.values() for w in widgets]
    assert not any(getattr(w, "text", lambda: "")() == "card" for w in cards)
    issue = window._banners.issue("plugins")
    assert issue.title == "Broken didn't start" and "no card today" in window._plugin_problems["broken"]


def test_an_added_plugin_that_exits_while_starting_is_left_out(window, plugins):
    _added(plugins, "quitter", header="raise SystemExit(3)")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    window.load_plugins(_builtins())
    assert window._plugin("quitter") is None and "SystemExit" in window._plugin_problems["quitter"]


def test_a_built_in_plugin_that_breaks_still_stops_the_start(window, plugins):
    class Broken(_Builtin):
        def setup(self, host, bus):
            raise RuntimeError("as before")

    with pytest.raises(RuntimeError):
        window.load_plugins([Broken("presets", "Presets")])


# ── The dialog ──────────────────────────────────────────────────────────────

@pytest.fixture
def answers(monkeypatch):
    """What the confirmation boxes answer (True: the person agreed), and what they asked."""
    asked = []
    reply = {"yes": True}

    def ask(_parent, title, text, _yes, danger=False):
        asked.append((title, text, danger))
        return reply["yes"]

    monkeypatch.setattr(dialog_module, "_ask", ask)
    return asked, reply


def _boxes(dialog) -> dict:
    return {box.accessibleName(): box for box in dialog.findChildren(QCheckBox)}


def _hosted(panel):
    """The panel with its footer placed under it, as the Settings window places it."""
    from PyQt6.QtWidgets import QVBoxLayout, QWidget
    holder = QWidget()
    lay = QVBoxLayout(holder)
    lay.addWidget(panel)
    lay.addWidget(panel.footer)
    panel.refresh()
    panel._holder = holder  # kept alive with the panel
    return panel


def _footer_shown(panel) -> bool:
    return not panel.footer.isHidden()


def test_switching_off_asks_first_names_what_goes_and_waits_for_a_restart(qapp, plugins, answers):
    asked, reply = answers
    builtins = _builtins()
    dialog = _hosted(PluginsPanel(builtins, {p.name for p in builtins}, {}, lambda: True))
    boxes = _boxes(dialog)
    assert boxes["Connection"].isChecked() and not boxes["Connection"].isEnabled()  # required: locked on
    assert not _footer_shown(dialog) and dialog.pending_changes() == 0
    reply["yes"] = False
    boxes["Presets"].setChecked(False)
    assert plugin_loader.builtin_off() == set() and boxes["Presets"].isChecked()  # cancelled: nothing changes
    reply["yes"] = True
    _boxes(dialog)["Presets"].setChecked(False)
    assert plugin_loader.builtin_off() == {"presets"}
    title, text, _danger = asked[-1]
    assert title == "Switch off Presets?" and "The Presets card goes away." in text and "settings stay saved" in text
    assert dialog.pending_changes() == 1 and _footer_shown(dialog)
    assert dialog._footer_note.text() == "Restart to apply 1 change"
    assert "Waits for a restart" in _labels(dialog)  # on the tile too
    _boxes(dialog)["Presets"].setChecked(True)
    assert plugin_loader.builtin_off() == set() and not _footer_shown(dialog)  # back as it is: no restart


def test_switching_on_an_added_plugin_warns_about_trust(qapp, plugins, answers):
    asked, _reply = answers
    _added(plugins, "extra")
    dialog = PluginsPanel(_builtins(), {"connection", "camera_control", "presets"}, {}, lambda: True)
    _boxes(dialog)["Extra"].setChecked(True)
    title, text, danger = asked[-1]
    assert title == "Switch on Extra?" and "trust" in text and danger
    assert plugin_loader.find_installed(folder=plugins)[0].enabled and dialog.pending_changes() == 1


def test_turn_all_back_on(qapp, plugins, answers):
    plugin_loader.set_builtin_off("presets", True)
    dialog = PluginsPanel(_builtins(), {"connection", "camera_control"}, {}, lambda: True)
    assert dialog._all_on_btn.isEnabled()
    dialog._all_on_btn.click()
    assert plugin_loader.builtin_off() == set() and not dialog._all_on_btn.isEnabled()


def test_restart_now_restarts(qapp, plugins, answers):
    restarted = []
    dialog = _hosted(PluginsPanel(_builtins(), set(), {}, lambda: restarted.append(True) or True))
    assert _footer_shown(dialog) and dialog._footer_note.text() == "Restart to apply 2 changes"
    dialog._restart_btn.click()
    assert restarted == [True]
    assert not dialog.findChildren(dialog_module.QPushButton, "row_restart")  # one button, in the footer


def test_a_plugin_that_didnt_start_counts_as_a_change_a_restart_applies(qapp, plugins, answers):
    # It says why on its tile, and a restart tries it again.
    _added(plugins, "extra")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    builtins = _builtins()
    dialog = PluginsPanel(builtins, {p.name for p in builtins}, {"extra": "RuntimeError: no card today"},
                          lambda: True)
    assert dialog.pending_changes() == 1


def test_a_plugin_that_didnt_start_says_why_in_the_list(qapp, plugins, answers):
    _added(plugins, "extra")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    dialog = PluginsPanel(_builtins(), set(), {"extra": "RuntimeError: no card today"}, lambda: True)
    texts = [label.text() for label in dialog.findChildren(dialog_module.QWidget) if hasattr(label, "text")
             and callable(label.text) and isinstance(label.text(), str)]
    assert any("It didn't start (RuntimeError: no card today)." in t for t in texts)


def test_one_thats_on_can_be_switched_off_even_with_a_problem(qapp, plugins, answers):
    folder = _added(plugins, "extra")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    (folder / "plugin.json").write_text((folder / "plugin.json").read_text().replace('"api": 1', '"api": 99'))
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)  # on, as this version
    dialog = PluginsPanel(_builtins(), {"connection", "camera_control", "presets", "extra"}, {}, lambda: True)
    box = _boxes(dialog)["Extra"]
    assert box.isEnabled() and box.isChecked()
    box.setChecked(False)
    assert not plugin_loader.find_installed(folder=plugins)[0].enabled


def test_switching_one_keeps_the_list_where_it_was(qapp, plugins, answers):
    for n in range(40):  # enough to overflow at any screen scaling
        _added(plugins, f"extra{n}")
    panel = PluginsPanel(_builtins(), {"connection", "camera_control", "presets"}, {}, lambda: True)
    win = SettingsWindow([SettingsSection("plugins", panel, on_show=panel.refresh)])
    win.resize(700, 500)
    win.show()
    bar = win._pages[0].verticalScrollBar()
    seen = -1
    for _ in range(40):  # the rows are rebuilt as the tab comes up, and laid out a pass later: wait for it to settle
        QTest.qWait(100)
        if bar.maximum() > 0 and bar.maximum() == seen:
            break
        seen = bar.maximum()
    bar.setValue(bar.maximum())
    at = bar.value()
    assert at > 0
    _boxes(panel)["Extra39"].setChecked(True)
    for _ in range(40):
        QTest.qWait(50)
        if bar.value() == at:
            break
    assert bar.value() == at
    win.close()


def test_a_plugin_being_written_shows_where_its_from_with_nothing_to_switch(qapp, plugins, answers, tmp_path,
                                                                          monkeypatch):
    work = tmp_path / "work"
    work.mkdir()
    folder = _added(work, "mine")
    monkeypatch.setenv("TELESCOPE_DEV_PROFILE", str(tmp_path / "profile"))
    monkeypatch.setenv("TELESCOPE_DEV_PLUGINS", str(folder))
    dialog = PluginsPanel(_builtins(), {"connection", "camera_control", "presets", "mine"}, {}, lambda: True)
    assert not _boxes(dialog)["Mine"].isEnabled()
    texts = [w.text() for w in dialog.findChildren(QLabel)]
    assert any(t.startswith(f"Loaded from {folder} by dev_desktop.py.") for t in texts)
    assert not [b for b in dialog.findChildren(dialog_module.QPushButton) if b.text() == "Remove"]
    assert dialog.pending_changes() == 0


def test_the_plugins_tab_opens_over_the_main_window_without_blocking_it(qapp, window, plugins):
    window.open_settings("plugins")
    win = window._settings_win
    try:
        assert win.parent() is window and win.isVisible() and not win.isModal()
        assert win.current_tab() == "plugins"
        window.open_settings("plugins")
        assert window._settings_win is win  # brought back up, not a second one
    finally:
        win.close()


# ── Optional built-in plugins ───────────────────────────────────────────────

class _Optional(_Builtin):
    optional = True

    def __init__(self, name, title, needs_vision=True):
        super().__init__(name, title)
        self.needs_vision = needs_vision


class _Download:
    """Stands in for VisionDownload: start() finishes at once, as the real one does once its thread is done."""

    def __init__(self, ready=False, available=True, error=""):
        from PyQt6.QtCore import QObject, pyqtSignal

        class Signals(QObject):
            progress = pyqtSignal(int, int)
            finished = pyqtSignal(str)
        self._signals = Signals()
        self.progress, self.finished = self._signals.progress, self._signals.finished
        self._ready, self._available, self._error = ready, available, error
        self.waiting = self.error = self.failed_for = ""
        self.started, self.removed = [], 0

    def ready(self):
        return self._ready

    def available(self):
        return self._available

    def running(self):
        return False

    def size_mb(self):
        return 47

    def start(self, switch_on=""):
        self.started.append(switch_on)
        if self._error:
            self.error, self.failed_for = self._error, switch_on
        else:
            self._ready = True
            plugin_loader.set_optional_on(switch_on, True)
        self.finished.emit(self._error)

    def remove(self):
        self.removed += 1
        self._ready = False


def test_an_optional_plugin_starts_off_and_isnt_loaded_until_its_switched_on(window, plugins):
    window.load_plugins([*_builtins(), _Optional("framing", "Auto framing")])
    assert "framing" not in [p.name for p in window._plugins]
    assert window.plugins_off() == []  # never switched on isn't "turned off"
    assert window._banners.issue("plugins_off") is None
    plugin_loader.set_optional_on("framing", True)
    plugin_loader.all_builtin_on()  # Turn all back on keeps it
    assert plugin_loader.optional_on() == {"framing"}


def test_switching_one_on_asks_to_download_first_then_switches_it_on(qapp, plugins, answers):
    asked, reply = answers
    dl = _Download()
    builtins = [*_builtins(), _Optional("framing", "Auto framing")]
    dialog = PluginsPanel(builtins, {"connection", "camera_control", "presets"}, {}, lambda: True,
                           download=dl)
    texts = [lbl.text() for lbl in dialog.findChildren(QLabel)]
    assert "Switching one of these on downloads about 47 MB first, once for all of them." in texts
    assert "Needs the vision download" in texts
    reply["yes"] = False
    _boxes(dialog)["Auto framing"].setChecked(True)
    assert dl.started == [] and plugin_loader.optional_on() == set()
    assert not _boxes(dialog)["Auto framing"].isChecked()
    reply["yes"] = True
    _boxes(dialog)["Auto framing"].setChecked(True)
    assert asked[-1][0] == "Download about 47 MB?" and "MediaPipe" in asked[-1][1]
    assert dl.started == ["framing"] and plugin_loader.optional_on() == {"framing"}
    assert _boxes(dialog)["Auto framing"].isChecked() and dialog.pending_changes() == 1
    assert dialog._remove_pack_btn.isHidden()  # something uses it


def test_with_the_download_there_switching_on_is_just_a_switch(qapp, plugins, answers):
    asked, _reply = answers
    dl = _Download(ready=True)
    builtins = [*_builtins(), _Optional("framing", "Auto framing"), _Optional("background", "Background")]
    dialog = PluginsPanel(builtins, {"connection"}, {}, lambda: True, download=dl)
    _boxes(dialog)["Background"].setChecked(True)
    assert asked == [] and dl.started == [] and plugin_loader.optional_on() == {"background"}
    _boxes(dialog)["Background"].setChecked(False)
    assert plugin_loader.optional_on() == set()
    assert not dialog._remove_pack_btn.isHidden()
    dialog._remove_pack_btn.click()
    assert dl.removed == 1


def test_a_failed_download_leaves_it_off_and_says_why(qapp, plugins, answers):
    dl = _Download(error="The download failed. Check the internet connection and try again.")
    dialog = PluginsPanel([*_builtins(), _Optional("framing", "Auto framing")], {"connection"}, {},
                           lambda: True, download=dl)
    _boxes(dialog)["Auto framing"].setChecked(True)
    assert plugin_loader.optional_on() == set() and not _boxes(dialog)["Auto framing"].isChecked()
    texts = [lbl.text() for lbl in dialog.findChildren(QLabel)]
    assert any(t.startswith("The download didn't work. The download failed.") for t in texts)


def test_without_a_pack_for_this_computer_it_cant_be_switched_on(qapp, plugins, answers):
    dl = _Download(available=False)
    dialog = PluginsPanel([*_builtins(), _Optional("framing", "Auto framing")], {"connection"}, {},
                           lambda: True, download=dl)
    assert not _boxes(dialog)["Auto framing"].isEnabled()


# ── Updating added plugins ─────────────────────────────────────────────────────

def _zip_of(folder, path):
    import zipfile
    with zipfile.ZipFile(path, "w") as zf:
        for f in folder.rglob("*"):
            zf.write(f, f.relative_to(folder.parent).as_posix())
    return path


def _labels(dialog) -> list:
    return [lbl.text() for lbl in dialog.findChildren(QLabel) if lbl.isVisibleTo(dialog)]  # not rows being replaced


def test_installing_a_plugin_thats_there_asks_then_replaces_it(qapp, plugins, answers, tmp_path, monkeypatch):
    asked, reply = answers
    shown = []
    monkeypatch.setattr(dialog_module.QMessageBox, "information", lambda _p, title, text: shown.append((title, text)))
    _added(plugins, "extra", version="1.0")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    newer = tmp_path / "new"
    newer.mkdir()
    _added(newer, "extra", version="1.1")
    archive = _zip_of(newer / "extra", tmp_path / "extra.zip")
    dialog = PluginsPanel(_builtins(), {"connection", "camera_control", "presets", "extra"}, {}, lambda: True)

    reply["yes"] = False
    dialog._install(str(archive))
    assert asked[-1][0] == "Replace Extra 1.0?" and "same settings" in asked[-1][1]
    assert plugin_loader.find_installed(folder=plugins)[0].version == "1.0"  # cancelled: untouched

    reply["yes"] = True
    dialog._install(str(archive))
    (item,) = plugin_loader.find_installed(folder=plugins)
    assert item.version == "1.1" and not item.enabled
    assert shown[-1][0] == "Replaced" and "Extra 1.1" in shown[-1][1]
    assert not _boxes(dialog)["Extra 1.1"].isChecked()
    assert "Waits for a restart" in _labels(dialog) and dialog.pending_changes() == 1  # the old one still runs


def test_the_repo_shows_as_a_link_next_to_the_author(qapp, plugins, answers):
    _added(plugins, "extra", author="Someone <b>", description="Does things.",
           repo="https://github.com/someone/extra")
    dialog = PluginsPanel(_builtins(), set(), {}, lambda: True)
    assert "Does things." in _labels(dialog)
    (by,) = [lbl for lbl in dialog.findChildren(QLabel) if lbl.text().startswith("By Someone")]
    assert by.text() == 'By Someone &lt;b&gt;. <a href="https://github.com/someone/extra">github.com/someone/extra</a>'
    assert by.openExternalLinks()


class _Updater(dialog_module.PluginUpdater):
    """An updater whose last check found release for every plugin, without asking anyone."""

    def __init__(self, release=None):
        super().__init__()
        self.release = release
        self.checks = 0

    def newer(self, item):
        return self.release if self.checking() else None

    def maybe_check(self):
        self.checks += 1


def test_a_newer_release_is_mentioned_with_a_link_to_it(qapp, plugins, answers):
    from telescope.plugin_updates import Release
    _added(plugins, "extra", version="1.0", repo="https://github.com/someone/extra")
    release = Release("v1.1", "https://github.com/someone/extra/releases/tag/v1.1")
    dialog = PluginsPanel(_builtins(), set(), {}, lambda: True, updater=_Updater(release))
    (note,) = [lbl for lbl in dialog.findChildren(QLabel) if "Version v1.1" in lbl.text()]
    assert "Version v1.1 is out." in note.text() and f'href="{release.page}"' in note.text()
    _boxes(dialog)["Check for updates"].setChecked(False)
    assert plugin_loader.plugin_switch("check_updates", True) is False
    assert not any("Version v1.1" in t for t in _labels(dialog))


def test_the_auto_update_switch_is_red_warns_and_asks_first(qapp, plugins, answers):
    asked, reply = answers
    _added(plugins, "extra")
    updater = _Updater()
    dialog = PluginsPanel(_builtins(), set(), {}, lambda: True, updater=updater)
    box = _boxes(dialog)["Install updates without asking"]
    assert box.property("danger") and not box.isChecked()
    assert any("Whoever controls a plugin's repo can then run new code" in t for t in _labels(dialog))

    reply["yes"] = False
    box.setChecked(True)
    assert asked[-1][2] and "every time they publish a release" in asked[-1][1]
    assert not box.isChecked() and plugin_loader.plugin_switch("auto_update", False) is False

    reply["yes"] = True
    _boxes(dialog)["Install updates without asking"].setChecked(True)
    assert plugin_loader.plugin_switch("auto_update", False) is True and updater.checks == 1
    count = len(asked)
    _boxes(dialog)["Install updates without asking"].setChecked(False)  # off again without asking
    assert len(asked) == count and plugin_loader.plugin_switch("auto_update", True) is False


def test_without_the_check_the_auto_switch_cant_go_on(qapp, plugins, answers):
    _added(plugins, "extra")
    plugin_loader.set_plugin_switch("check_updates", False)
    dialog = PluginsPanel(_builtins(), set(), {}, lambda: True, updater=_Updater())
    assert not _boxes(dialog)["Install updates without asking"].isEnabled()


def test_with_no_added_plugins_there_are_no_update_switches(qapp, plugins, answers):
    dialog = PluginsPanel(_builtins(), set(), {}, lambda: True, updater=_Updater())
    assert "Check for updates" not in _boxes(dialog)


# ── Finding a plugin: groups, order, search and the On/Off filter ──────────────

def _grouped():
    camera = _Builtin("zoom_tool", "Zoom tool")
    camera.category, camera.about = "camera", "Makes the picture bigger."
    other = _Builtin("alpha", "Alpha")
    other.category = "camera"
    sound = _Builtin("echo", "Echo")
    sound.category = "sound"
    lost = _Builtin("lost", "Lost")
    lost.category = "no_such_group"
    return [*_builtins(), camera, other, sound, lost]


def _group_names(panel) -> list:
    return [f.findChild(QLabel, "card_title").text() for f, _tiles, _count in panel._groups if not f.isHidden()]


def test_tiles_are_grouped_by_category_and_sorted_by_name(qapp, plugins, answers):
    panel = PluginsPanel(_grouped(), set(), {}, lambda: True)
    assert _group_names(panel) == ["Camera", "Sound", "Extras"]  # empty groups aren't shown
    assert panel.shown_tiles() == ["Alpha", "Zoom tool", "Echo", "Camera", "Connection", "Lost", "Presets"]


def test_the_search_finds_by_name_and_description_in_the_language_shown(qapp, plugins, answers, monkeypatch):
    real = dialog_module.tr
    german = {"Presets": "Voreinstellungen", "What Presets does.": "Speichert Einstellungen."}
    monkeypatch.setattr(dialog_module, "tr", lambda t, /, *a, **k: german.get(t) or real(t, *a, **k))
    panel = PluginsPanel(_grouped(), set(), {}, lambda: True)
    panel._search.setText("vorein")
    assert panel.shown_tiles() == ["Voreinstellungen"] and _group_names(panel) == ["Extras"]
    panel._search.setText("speichert")
    assert panel.shown_tiles() == ["Voreinstellungen"]
    panel._search.setText("picture bigger")  # every word, anywhere in the name or the description
    assert panel.shown_tiles() == ["Zoom tool"]
    assert panel._added_box.isHidden() and panel._empty.isHidden()
    panel._search.setText("nothing like it")
    assert panel.shown_tiles() == [] and not panel._empty.isHidden()
    assert panel._empty_note.text() == "No plugins match “nothing like it”."
    panel._show_all_btn.click()
    assert panel._search.text() == "" and len(panel.shown_tiles()) == 7 and not panel._added_box.isHidden()


def test_the_filter_shows_the_ones_on_or_off(qapp, plugins, answers):
    plugin_loader.set_builtin_off("presets", True)
    plugin_loader.set_builtin_off("echo", True)
    panel = PluginsPanel(_grouped(), set(), {}, lambda: True)
    panel._filter_btns["off"].click()
    assert panel.shown_tiles() == ["Echo", "Presets"] and panel._filter_btns["off"].isChecked()
    panel._filter_btns["on"].click()
    assert panel.shown_tiles() == ["Alpha", "Zoom tool", "Camera", "Connection", "Lost"]  # a required one is on
    panel._search.setText("echo")
    assert panel.shown_tiles() == [] and panel._empty_note.text() == "No plugins match “echo”."
    panel._search.clear()
    plugin_loader.all_builtin_on()
    panel._set_filter("off")
    panel.refresh()
    assert panel.shown_tiles() == [] and panel._empty_note.text() == "Every plugin is switched on."


def test_the_footer_counts_the_changes_that_wait_for_a_restart(qapp, plugins, answers):
    builtins = _grouped()
    panel = _hosted(PluginsPanel(builtins, {p.name for p in builtins}, {}, lambda: True))
    assert not _footer_shown(panel)
    _boxes(panel)["Presets"].setChecked(False)
    assert _footer_shown(panel) and panel._footer_note.text() == "Restart to apply 1 change"
    _boxes(panel)["Echo"].setChecked(False)
    assert panel._footer_note.text() == "Restart to apply 2 changes" and panel.pending_changes() == 2
    panel._all_on_btn.click()
    assert not _footer_shown(panel) and panel.pending_changes() == 0


def test_the_footer_counts_a_removed_plugin_that_still_runs(qapp, plugins, answers):
    panel = _hosted(PluginsPanel(_builtins(), {"connection", "camera_control", "presets", "gone"}, {}, lambda: True))
    assert _footer_shown(panel) and panel.pending_changes() == 1


def test_a_footer_with_nowhere_to_go_never_becomes_a_window(qapp, plugins, answers):
    panel = PluginsPanel(_builtins(), set(), {}, lambda: True)
    assert panel.pending_changes() == 2 and panel.footer.isHidden() and panel.footer.parentWidget() is None


def test_required_plugins_are_locked_on_and_say_so(qapp, plugins, answers):
    panel = PluginsPanel(_builtins(), set(), {}, lambda: True)
    box = _boxes(panel)["Connection"]
    assert box.isChecked() and not box.isEnabled()
    tile = box.parentWidget()
    assert "Required" in [lbl.text() for lbl in tile.findChildren(QLabel)]
    assert "can't be switched off" in tile.toolTip()


def test_an_added_plugin_goes_in_the_group_its_plugin_json_names(qapp, plugins, answers):
    _added(plugins, "extra", category="sound")
    _added(plugins, "other", category="made_up")
    panel = PluginsPanel(_builtins(), set(), {}, lambda: True)
    groups = {f.findChild(QLabel, "card_title").text(): [t.findChild(QLabel, "setting_name").text() for t in tiles]
              for f, tiles, _count in panel._groups}
    assert groups["Sound"] == ["Extra"] and "Other" in groups["Extras"]
    assert "Added" in [lbl.text() for lbl in panel.findChildren(QLabel)]


def test_every_built_in_plugin_picks_its_category_and_icon():
    from telescope.plugin import PLUGIN_CATEGORIES
    from telescope.plugins import builtin_plugins
    from telescope.widgets.common import _ICON_SVG
    from telescope.widgets.line_icons import LINE_ICONS
    for plugin in builtin_plugins():
        own = vars(type(plugin))
        assert own.get("category") in dict(PLUGIN_CATEGORIES), f"{plugin.name} needs a category"  # not Extras by default
        assert own.get("icon") in LINE_ICONS or own.get("icon") in _ICON_SVG, f"{plugin.name} needs an icon"


def _card_headers():
    """(file, class, method, call) for every add_card_header in the built-in plugins."""
    from pathlib import Path
    for path in sorted(Path(dialog_module.__file__).parents[1].joinpath("plugins").glob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for cls in (n for n in tree.body if isinstance(n, ast.ClassDef)):
            for method in (n for n in cls.body if isinstance(n, ast.FunctionDef)):
                for call in ast.walk(method):
                    if isinstance(call, ast.Call) and getattr(call.func, "id", "") == "add_card_header":
                        yield path.name, cls.name, method.name, call


def test_every_card_takes_its_plugins_group_colour_and_a_main_card_its_icon():
    from telescope.plugin import PLUGIN_CATEGORIES
    groups = {cid for cid, _name in PLUGIN_CATEGORIES}
    found = 0
    for file, cls, method, call in _card_headers():
        hue = next((k.value for k in call.keywords if k.arg == "hue"), None)
        where = f"{file} {cls}.{method}"
        assert hue is not None, f"{where}: a card without hue= is the app's purple instead of its group's colour"
        literal = hue.value if isinstance(hue, ast.Constant) else None
        assert literal in groups if literal is not None else getattr(hue, "attr", "") == "category", where
        if method == "create_panel":
            found += 1
            assert getattr(hue, "attr", "") == "category" and getattr(call.args[2], "attr", "") == "icon", \
                f"{where}: the window's card has the same icon and colour as the plugin's tile"
    assert found > 15


def test_side_columns_put_the_pinned_cards_first_then_sort_by_group(window):
    from PyQt6.QtWidgets import QWidget
    from telescope.plugin import TelescopePlugin

    def card(name, category, pinned=False, region="right"):
        cls = type(name, (TelescopePlugin,), {"name": name, "category": category, "pinned": pinned,
                                              "panel_region": region, "create_panel": lambda self: QWidget()})
        window.register_plugin(cls())

    for name, category, pinned in (("fun", "look", False), ("photos", "camera", False), ("tag", "on_screen", False),
                                   ("camera", "camera", True), ("grade", "look", False), ("odd", "nowhere", False)):
        card(name, category, pinned)
    keys = [window._panel_order[id(panel)] for panel in window._panels["right"]]
    assert keys == [(0, 0), (1, 0), (1, 1), (1, 1), (1, 4), (1, 8)]
