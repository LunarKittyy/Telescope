"""Switching plugins on and off: what the window loads, what it says, and the Plugins dialog."""

import json
import sys

import pytest
from PyQt6.QtWidgets import QCheckBox, QLabel, QMenu

import telescope.widgets.plugins_dialog as dialog_module
from telescope import plugin_loader
from telescope.widgets.plugins_dialog import PluginsDialog
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


def test_the_settings_menu_says_how_many_are_off(window, plugins, monkeypatch):
    plugin_loader.set_builtin_off("presets", True)
    plugin_loader.set_builtin_off("camera_control", True)
    window.load_plugins(_builtins())
    shown = []
    monkeypatch.setattr(QMenu, "exec", lambda self, *_a: shown.append([a.text() for a in self.actions()]))
    window._show_settings_menu()
    assert shown == [["Plugins… (2 off)"]]


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


def test_switching_off_asks_first_names_what_goes_and_waits_for_a_restart(qapp, plugins, answers):
    asked, reply = answers
    builtins = _builtins()
    dialog = PluginsDialog(None, builtins, {p.name for p in builtins}, {}, lambda: True)
    boxes = _boxes(dialog)
    assert "Connection" not in boxes  # required: no switch at all
    assert dialog._restart_bar.isHidden()
    reply["yes"] = False
    boxes["Presets"].setChecked(False)
    assert plugin_loader.builtin_off() == set() and boxes["Presets"].isChecked()  # cancelled: nothing changes
    reply["yes"] = True
    _boxes(dialog)["Presets"].setChecked(False)
    assert plugin_loader.builtin_off() == {"presets"}
    title, text, _danger = asked[-1]
    assert title == "Switch off Presets?" and "The Presets card goes away." in text and "settings stay saved" in text
    assert not dialog._restart_bar.isHidden()
    _boxes(dialog)["Presets"].setChecked(True)
    assert plugin_loader.builtin_off() == set() and dialog._restart_bar.isHidden()  # back as it is: no restart


def test_switching_on_an_added_plugin_warns_about_trust(qapp, plugins, answers):
    asked, _reply = answers
    _added(plugins, "extra")
    dialog = PluginsDialog(None, _builtins(), {"connection", "camera_control", "presets"}, {}, lambda: True)
    _boxes(dialog)["Extra"].setChecked(True)
    title, text, danger = asked[-1]
    assert title == "Switch on Extra?" and "trust" in text and danger
    assert plugin_loader.find_installed(folder=plugins)[0].enabled and not dialog._restart_bar.isHidden()


def test_turn_all_back_on(qapp, plugins, answers):
    plugin_loader.set_builtin_off("presets", True)
    dialog = PluginsDialog(None, _builtins(), {"connection", "camera_control"}, {}, lambda: True)
    assert dialog._all_on_btn.isEnabled()
    dialog._all_on_btn.click()
    assert plugin_loader.builtin_off() == set() and not dialog._all_on_btn.isEnabled()


def test_restart_now_restarts(qapp, plugins, answers):
    restarted = []
    dialog = PluginsDialog(None, _builtins(), set(), {}, lambda: restarted.append(True) or True)
    dialog._restart_btn.click()
    assert restarted == [True]


def test_a_plugin_that_didnt_start_says_why_in_the_list(qapp, plugins, answers):
    _added(plugins, "extra")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    dialog = PluginsDialog(None, _builtins(), set(), {"extra": "RuntimeError: no card today"}, lambda: True)
    texts = [label.text() for label in dialog.findChildren(dialog_module.QWidget) if hasattr(label, "text")
             and callable(label.text) and isinstance(label.text(), str)]
    assert any("It didn't start (RuntimeError: no card today)." in t for t in texts)


def test_one_thats_on_can_be_switched_off_even_with_a_problem(qapp, plugins, answers):
    folder = _added(plugins, "extra")
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)
    (folder / "plugin.json").write_text((folder / "plugin.json").read_text().replace('"api": 1', '"api": 99'))
    plugin_loader.set_enabled(plugin_loader.find_installed(folder=plugins)[0], True)  # on, as this version
    dialog = PluginsDialog(None, _builtins(), {"connection", "camera_control", "presets", "extra"}, {}, lambda: True)
    box = _boxes(dialog)["Extra"]
    assert box.isEnabled() and box.isChecked()
    box.setChecked(False)
    assert not plugin_loader.find_installed(folder=plugins)[0].enabled


def test_switching_one_keeps_the_list_where_it_was(qapp, plugins, answers):
    for n in range(40):  # enough to overflow at any screen scaling
        _added(plugins, f"extra{n}")
    dialog = PluginsDialog(None, _builtins(), {"connection", "camera_control", "presets"}, {}, lambda: True)
    dialog.resize(500, 400)
    dialog.show()
    qapp.processEvents()
    bar = dialog._scroll.verticalScrollBar()
    bar.setValue(bar.maximum())
    at = bar.value()
    assert at > 0
    _boxes(dialog)["Extra39"].setChecked(True)
    qapp.processEvents()
    assert bar.value() == at
    dialog.close()


def test_a_plugin_being_written_shows_where_its_from_with_nothing_to_switch(qapp, plugins, answers, tmp_path,
                                                                          monkeypatch):
    work = tmp_path / "work"
    work.mkdir()
    folder = _added(work, "mine")
    monkeypatch.setenv("TELESCOPE_DEV_PROFILE", str(tmp_path / "profile"))
    monkeypatch.setenv("TELESCOPE_DEV_PLUGINS", str(folder))
    dialog = PluginsDialog(None, _builtins(), {"connection", "camera_control", "presets", "mine"}, {}, lambda: True)
    assert "Mine" not in _boxes(dialog)
    texts = [w.text() for w in dialog.findChildren(QLabel)]
    assert any(t.startswith(f"Loaded from {folder} by dev_desktop.py.") for t in texts)
    assert not [b for b in dialog.findChildren(dialog_module.QPushButton) if b.text() == "Remove"]
    assert dialog._restart_bar.isHidden()


def test_the_plugins_window_opens_over_the_main_window_without_blocking_it(qapp, window, plugins):
    window.show_plugins()
    dlg = window._plugins_dlg
    try:
        assert dlg.parent() is window and dlg.isVisible() and not dlg.isModal()
        window.show_plugins()
        assert window._plugins_dlg is dlg  # brought back up, not a second one
    finally:
        dlg.close()


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
    dialog = PluginsDialog(None, builtins, {"connection", "camera_control", "presets"}, {}, lambda: True,
                           download=dl)
    texts = [lbl.text() for lbl in dialog.findChildren(QLabel)]
    assert "Switching it on downloads about 47 MB first." in texts
    reply["yes"] = False
    _boxes(dialog)["Auto framing"].setChecked(True)
    assert dl.started == [] and plugin_loader.optional_on() == set()
    assert not _boxes(dialog)["Auto framing"].isChecked()
    reply["yes"] = True
    _boxes(dialog)["Auto framing"].setChecked(True)
    assert asked[-1][0] == "Download about 47 MB?" and "MediaPipe" in asked[-1][1]
    assert dl.started == ["framing"] and plugin_loader.optional_on() == {"framing"}
    assert _boxes(dialog)["Auto framing"].isChecked() and not dialog._restart_bar.isHidden()
    assert dialog._remove_pack_btn.isHidden()  # something uses it


def test_with_the_download_there_switching_on_is_just_a_switch(qapp, plugins, answers):
    asked, _reply = answers
    dl = _Download(ready=True)
    builtins = [*_builtins(), _Optional("framing", "Auto framing"), _Optional("background", "Background")]
    dialog = PluginsDialog(None, builtins, {"connection"}, {}, lambda: True, download=dl)
    _boxes(dialog)["Background"].setChecked(True)
    assert asked == [] and dl.started == [] and plugin_loader.optional_on() == {"background"}
    _boxes(dialog)["Background"].setChecked(False)
    assert plugin_loader.optional_on() == set()
    assert not dialog._remove_pack_btn.isHidden()
    dialog._remove_pack_btn.click()
    assert dl.removed == 1


def test_a_failed_download_leaves_it_off_and_says_why(qapp, plugins, answers):
    dl = _Download(error="The download failed. Check the internet connection and try again.")
    dialog = PluginsDialog(None, [*_builtins(), _Optional("framing", "Auto framing")], {"connection"}, {},
                           lambda: True, download=dl)
    _boxes(dialog)["Auto framing"].setChecked(True)
    assert plugin_loader.optional_on() == set() and not _boxes(dialog)["Auto framing"].isChecked()
    texts = [lbl.text() for lbl in dialog.findChildren(QLabel)]
    assert any(t.startswith("The download didn't work. The download failed.") for t in texts)


def test_without_a_pack_for_this_computer_it_cant_be_switched_on(qapp, plugins, answers):
    dl = _Download(available=False)
    dialog = PluginsDialog(None, [*_builtins(), _Optional("framing", "Auto framing")], {"connection"}, {},
                           lambda: True, download=dl)
    assert not _boxes(dialog)["Auto framing"].isEnabled()
