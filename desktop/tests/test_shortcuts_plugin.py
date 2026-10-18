import pytest
from PyQt6.QtCore import QEvent, Qt
from PyQt6.QtGui import QKeyEvent, QKeySequence
from PyQt6.QtTest import QTest
from PyQt6.QtWidgets import QApplication, QCheckBox, QLineEdit, QPushButton, QVBoxLayout, QWidget

from telescope.platform.hotkeys import HotkeyBackend, HotkeyStatus
from telescope.plugins import shortcuts as shortcuts_module
from telescope.plugins.shortcuts import BindingEditor, ShortcutsDialog, ShortcutsPlugin, action_command
from telescope.shortcuts import button_action, slider_action
from telescope.widgets.common import NoScrollSlider


class _Host:
    def __init__(self, actions=()):
        self.actions = list(actions)
        self.saves = 0
        self.notes = []
        self.streaming = False
        self.camera_on = True
        self.calls = []

    def shortcut_actions(self):
        return self.actions

    def schedule_save(self):
        self.saves += 1

    def send_notification(self, title, body, urgent=True):
        self.notes.append((title, body, urgent))

    def is_streaming(self):
        return self.streaming

    def is_starting(self):
        return False

    def start_stream(self, interactive=True):
        self.calls.append(("start", interactive))
        self.streaming = True

    def stop_all_streams(self):
        self.calls.append(("stop",))
        self.streaming = False

    def is_camera_on(self):
        return self.camera_on

    def can_turn_camera_off(self):
        return True, ""

    def set_camera_on(self, on, auto=False):
        self.camera_on = on

    def toggle_window(self):
        self.calls.append(("window",))
        return len([c for c in self.calls if c == ("window",)]) % 2 == 1


class _Backend(HotkeyBackend):
    def __init__(self):
        super().__init__()
        self.applied = []
        self.statuses = {}
        self.stopped = False

    def apply(self, hotkeys):
        self.applied.append(list(hotkeys))

    def status(self, hotkey_id):
        return self.statuses.get(hotkey_id)

    def stop(self):
        self.stopped = True


@pytest.fixture
def card(qapp):
    w = QWidget()
    lay = QVBoxLayout(w)
    mute = QPushButton("Mute")
    mute.setCheckable(True)
    torch = QCheckBox("On")
    slider = NoScrollSlider(Qt.Orientation.Horizontal)
    slider.setRange(0, 100)
    entry = QLineEdit()
    for x in (mute, torch, slider, entry):
        lay.addWidget(x)
    w.mute, w.torch, w.slider, w.entry = mute, torch, slider, entry
    yield w
    w.close()
    w.deleteLater()


@pytest.fixture
def make(card, qapp):
    made = []

    def build(bindings=(), backend="fake", notify=False):
        host = _Host([
            button_action("microphone.mute", "Mute", "Microphone", card.mute, states=("muted", "unmuted")),
            button_action("camera.torch", "Torch", "Camera", card.torch),
            slider_action("transforms.zoom", "Zoom", "Transforms", card.slider, step=10),
        ])
        fake = _Backend() if backend == "fake" else None
        plugin = ShortcutsPlugin(backend_factory=lambda: fake)
        plugin.setup(host, None)
        host.actions += plugin.create_actions()  # the window collects every plugin's, its own included
        plugin.set_config({"bindings": list(bindings), "notify": notify})
        plugin.start()
        made.append(plugin)
        return plugin, host, fake

    yield build
    for plugin in made:
        plugin.shutdown()


def _b(bid, keys, action, params=None, glob=False):
    return {"id": bid, "keys": keys, "action": action, "params": params or {}, "global": glob}


def test_start_collects_actions_app_ones_first_and_gives_controls_a_menu(make, card):
    plugin, _host, _ = make()
    ids = [a.id for a in plugin.actions()]
    assert ids[:3] == ["streaming.start_stop", "streaming.camera", "window.show_hide"]
    assert ids[3:] == ["microphone.mute", "camera.torch", "transforms.zoom"]
    assert card.mute.contextMenuPolicy() == Qt.ContextMenuPolicy.CustomContextMenu


def test_only_global_bindings_without_clashes_go_to_the_system(make):
    plugin, _host, backend = make([
        _b("a", "Ctrl+Alt+M", "microphone.mute", glob=True),
        _b("b", "Ctrl+T", "camera.torch"),
        _b("c", "Ctrl+Alt+M", "camera.torch", glob=True),  # same keys as a
        _b("d", "Ctrl+Alt+X", "gone.action", glob=True),  # not in this version
    ])
    hotkeys = backend.applied[-1]
    assert [(h.id, h.keys) for h in hotkeys] == [("a", "Ctrl+Alt+M")]
    assert hotkeys[0].description == "Mute: switch on or off"
    assert plugin.where(plugin.bindings[1]) == ("In Telescope", True)
    assert plugin.where(plugin.bindings[2]) == ("Key used by a shortcut above", False)
    assert plugin.where(plugin.bindings[3]) == ("Not in this version of Telescope", False)
    assert plugin.where(plugin.bindings[0]) == ("Everywhere (setting up)", True)
    backend.statuses["a"] = HotkeyStatus(False, "Another app already uses this key")
    assert plugin.where(plugin.bindings[0]) == ("In Telescope: Another app already uses this key", False)
    backend.statuses["a"] = HotkeyStatus(False, "Another app already uses this key", reaches_app=False)
    assert plugin.where(plugin.bindings[0]) == ("Another app already uses this key", False)


def test_a_global_press_runs_the_action_and_hold_comes_back_on_release(make, card):
    _plugin, _host, backend = make([_b("a", "Ctrl+Alt+M", "microphone.mute", {"mode": "hold"}, glob=True)])
    card.mute.setChecked(True)
    backend.pressed.emit("a")
    assert not card.mute.isChecked()
    backend.released.emit("a")
    assert card.mute.isChecked()


def test_a_background_press_notifies_only_when_asked(make, card, monkeypatch):
    monkeypatch.setattr(QApplication, "activeWindow", staticmethod(lambda: None))
    plugin, host, backend = make([_b("a", "F9", "camera.torch", glob=True)], notify=True)
    backend.pressed.emit("a")
    assert host.notes == [("Telescope", "Torch: on", False)]
    plugin.set_notify(False)
    backend.released.emit("a")
    backend.pressed.emit("a")
    assert len(host.notes) == 1
    assert not card.torch.isChecked()


def test_keys_inside_telescope_run_bindings_and_skip_text_boxes(make, card):
    plugin, _host, backend = make([
        _b("a", "Ctrl+T", "camera.torch"),
        _b("b", "Z", "transforms.zoom", {"op": "up", "amount": 10}),
        _b("c", "Ctrl+Alt+M", "microphone.mute", glob=True),
    ])
    card.show()
    card.activateWindow()
    QTest.qWaitForWindowActive(card)
    card.mute.setFocus()
    QTest.keyClick(card.mute, Qt.Key.Key_T, Qt.KeyboardModifier.ControlModifier)
    assert card.torch.isChecked()
    QTest.keyClick(card.mute, Qt.Key.Key_Z)
    assert card.slider.value() == 10
    card.entry.setFocus()
    QTest.keyClick(card.entry, Qt.Key.Key_Z)  # typing a z
    assert card.slider.value() == 10
    assert card.entry.text() == "z"
    QTest.keyClick(card.entry, Qt.Key.Key_T, Qt.KeyboardModifier.ControlModifier)  # with Ctrl it's still ours
    assert not card.torch.isChecked()
    # A global one works inside too: the desktop may have bound another key for it
    card.mute.setFocus()
    QTest.keyClick(card.mute, Qt.Key.Key_M, Qt.KeyboardModifier.ControlModifier | Qt.KeyboardModifier.AltModifier)
    assert card.mute.isChecked()


def test_one_key_press_seen_globally_and_inside_runs_once(make, card):
    plugin, _host, backend = make([_b("c", "Ctrl+Alt+M", "microphone.mute", glob=True)])
    card.show()
    QTest.qWaitForWindowActive(card)
    card.torch.setFocus()
    backend.pressed.emit("c")
    QTest.keyClick(card.torch, Qt.Key.Key_M, Qt.KeyboardModifier.ControlModifier | Qt.KeyboardModifier.AltModifier)
    backend.released.emit("c")
    assert card.mute.isChecked()


def test_holding_a_key_repeats_steps_but_not_toggles(make, card):
    plugin, _host, _ = make([_b("a", "Z", "transforms.zoom", {"op": "up", "amount": 5}),
                             _b("b", "T", "camera.torch")])
    card.show()
    QTest.qWaitForWindowActive(card)
    card.mute.setFocus()
    for repeat in (False, True, True):
        QApplication.sendEvent(card.mute, QKeyEvent(QEvent.Type.KeyPress, Qt.Key.Key_Z,
                                                    Qt.KeyboardModifier.NoModifier, "z", repeat))
        QApplication.sendEvent(card.mute, QKeyEvent(QEvent.Type.KeyPress, Qt.Key.Key_T,
                                                    Qt.KeyboardModifier.NoModifier, "t", repeat))
    assert card.slider.value() == 15
    assert card.torch.isChecked()


def test_hold_inside_telescope_comes_back_when_the_key_is_let_go(make, card):
    plugin, _host, _ = make([_b("a", "Ctrl+Space", "microphone.mute", {"mode": "hold"})])
    card.show()
    QTest.qWaitForWindowActive(card)
    card.torch.setFocus()
    QTest.keyPress(card.torch, Qt.Key.Key_Space, Qt.KeyboardModifier.ControlModifier)
    assert card.mute.isChecked()
    QTest.keyRelease(card.torch, Qt.Key.Key_Space)  # Ctrl may well come up first
    assert not card.mute.isChecked()


def test_remote_actions(make, card):
    plugin, host, _ = make()
    listing = plugin.handle_remote({"list": True})
    assert listing["ok"]
    assert "microphone.mute  (Microphone: Mute)" in listing["text"]
    assert "    mode=toggle | on | off | hold  (default toggle)" in listing["text"]
    assert "    amount=<number 0 to 100>" in listing["text"]
    assert plugin.handle_remote({"action": "camera.torch", "params": {"mode": "on"}}) == {
        "ok": True, "text": "Torch: on"}
    assert plugin.handle_remote({"action": "transforms.zoom", "params": {"op": "set", "value": "40"}})["ok"]
    assert card.slider.value() == 40
    assert not plugin.handle_remote({"action": "nope"})["ok"]
    assert "no setting 'colour'" in plugin.handle_remote({"action": "camera.torch",
                                                          "params": {"colour": "red"}})["text"]
    card.torch.setEnabled(False)
    assert not plugin.handle_remote({"action": "camera.torch"})["ok"]
    assert plugin.handle_remote({"action": "streaming.start_stop"}) == {"ok": True, "text": "Streaming: starting"}
    assert plugin.handle_remote({"action": "streaming.start_stop", "params": {"mode": "start"}})["text"] == \
        "Already streaming"
    assert plugin.handle_remote({"action": "streaming.start_stop"})["text"] == "Streaming: stopped"
    assert host.calls[-1] == ("stop",)
    assert plugin.handle_remote({"action": "streaming.camera", "params": {"mode": "off"}})["text"] == \
        "Phone camera: off"
    assert plugin.handle_remote({"action": "window.show_hide"}) == {"ok": True, "text": "Telescope: shown"}
    assert plugin.handle_remote({"action": "window.show_hide"}) == {"ok": True, "text": "Telescope: hidden"}


@pytest.mark.parametrize("params, says", [
    ({"op": "dwon"}, "op can be up | down | set, not 'dwon'"),
    ({"amount": "lots"}, "amount has to be a number, not 'lots'"),
    ({"op": "set"}, "needs value="),
])
def test_remote_settings_are_checked_not_defaulted(make, card, params, says):
    plugin, _host, _ = make()
    reply = plugin.handle_remote({"action": "transforms.zoom", "params": params})
    assert not reply["ok"] and says in reply["text"]
    assert card.slider.value() == 0


def test_remote_hold_is_refused(make, card):
    plugin, _host, _ = make()
    reply = plugin.handle_remote({"action": "microphone.mute", "params": {"mode": "hold"}})
    assert not reply["ok"] and "hold" in reply["text"]
    assert not card.mute.isChecked()


def test_config_round_trip_and_editing(make):
    plugin, host, backend = make()
    plugin.set_bindings([_b("a", "ctrl+f1", "camera.torch", glob=True)])
    assert host.saves == 1
    assert plugin.get_config() == {"bindings": [_b("a", "Ctrl+F1", "camera.torch", glob=True)], "notify": False}
    assert [h.id for h in backend.applied[-1]] == ["a"]
    plugin.set_bindings(plugin.bindings + [_b("x", "F3", "camera.torch")])  # not global: nothing to rebind
    assert len(backend.applied) == 2
    plugin.remove_binding("a")
    assert [b["id"] for b in plugin.get_config()["bindings"]] == ["x"]
    assert backend.applied[-1] == []
    plugin.shutdown()
    assert backend.stopped


def test_without_a_backend_global_bindings_still_work_inside(make):
    plugin, _host, _ = make([_b("a", "Ctrl+F1", "camera.torch", glob=True)], backend=None)
    assert plugin.where(plugin.bindings[0]) == ("In Telescope (no global shortcuts here)", False)
    assert plugin.binding_for_keys("Ctrl+F1")["id"] == "a"


def test_editor_builds_a_binding(make, card):
    plugin, _host, _ = make([_b("a", "Ctrl+T", "camera.torch")])
    card.slider.setValue(30)
    editor = BindingEditor(plugin, {"id": "new1", "keys": "", "action": "transforms.zoom", "params": {},
                                    "global": True})
    try:
        assert not editor._save_btn.isEnabled()  # no key yet
        _op, op_box, _ = editor._param_widgets["op"]
        op_box.setCurrentIndex(op_box.findData("set"))
        _p, value_box, value_row = editor._param_widgets["value"]
        assert value_box.value() == 30  # starts where the control is
        assert not editor._param_widgets["amount"][2].isVisibleTo(editor)
        assert value_row.isVisibleTo(editor)
        editor._keys_edit.setKeySequence(QKeySequence("Ctrl+T"))
        assert not editor._save_btn.isEnabled()
        assert "already does" in editor._problem.text()
        editor._keys_edit.setKeySequence(QKeySequence("Q"))
        assert editor._save_btn.isEnabled()  # allowed, with a warning, everywhere
        assert "stop typing" in editor._problem.text()
        editor._keys_edit.setKeySequence(QKeySequence("Ctrl+Shift+Z"))
        assert editor._problem.isHidden()
        assert editor.binding() == {"id": "new1", "keys": "Ctrl+Shift+Z", "action": "transforms.zoom",
                                    "params": {"op": "set", "amount": 10.0, "value": 30.0}, "global": True}
    finally:
        editor.deleteLater()


def test_editor_keeps_an_option_that_is_gone(make):
    plugin, _host, _ = make()
    editor = BindingEditor(plugin, {"id": "x", "keys": "F2", "action": "camera.torch",
                                    "params": {"mode": "on"}, "global": False})
    try:
        assert editor.binding()["params"] == {"mode": "on"}
        assert editor.binding()["global"] is False
    finally:
        editor.deleteLater()


def test_dialog_lists_bindings(make, monkeypatch):
    monkeypatch.setattr(shortcuts_module, "IS_LINUX", True)
    plugin, _host, backend = make([_b("a", "Ctrl+Alt+M", "microphone.mute", glob=True), _b("b", "F2", "camera.torch")])
    backend.statuses["a"] = HotkeyStatus(True, "Everywhere: Meta+M")
    dialog = ShortcutsDialog(plugin)
    try:
        dialog.refresh()
        assert dialog._list.count() == 2
        assert not dialog._edit_btn.isEnabled()
        dialog._list.setCurrentRow(1)
        assert dialog._remove_btn.isEnabled()
        dialog._remove()
        assert [b["id"] for b in plugin.bindings] == ["a"]
        dialog.refresh()
        assert dialog._list.count() == 1
    finally:
        dialog.deleteLater()


def test_action_command(monkeypatch):
    cmd = action_command("transforms.zoom", {"op": "up", "amount": 0.25})
    assert cmd[-4:] == ["--action", "transforms.zoom", "op=up", "amount=0.25"]
    assert cmd[1].endswith("main.py")


def test_editor_search_narrows_the_controls(make):
    plugin, _host, _ = make()
    editor = BindingEditor(plugin, {"id": "n", "keys": "", "action": "", "params": {}, "global": True})
    try:
        combo = editor._action_combo
        listed = lambda: [combo.itemData(i) for i in range(combo.count()) if combo.itemData(i)]  # noqa: E731
        assert len(listed()) == len(plugin.actions())
        assert editor._search.hasFocus() or QApplication.focusWidget() is None  # a new one starts at the search
        editor._search.setText("torch")
        assert listed() == ["camera.torch"]
        assert combo.currentData() == "camera.torch"
        assert "mode" in editor._param_widgets
        editor._search.setText("push talk")  # what a setting can do counts too
        assert listed() == []
        editor._search.setText("held")
        assert set(listed()) == {"microphone.mute", "camera.torch"}
        assert combo.currentData() == "camera.torch"  # still there, so still picked, settings kept
        editor._search.setText("zzz")
        assert listed() == [] and editor._problem.text() == "No control matches that search."
        assert not editor._save_btn.isEnabled()
        editor._search.clear()
        assert len(listed()) == len(plugin.actions())
    finally:
        editor.deleteLater()


def test_editor_search_uses_keywords(make):
    plugin, _host, _ = make()
    plugin.action("transforms.zoom").keywords = "crop framing"
    editor = BindingEditor(plugin, {"id": "n", "keys": "", "action": "", "params": {}, "global": True})
    try:
        editor._search.setText("crop")
        assert editor._action_combo.currentData() == "transforms.zoom"
    finally:
        editor.deleteLater()


def test_a_key_whose_control_is_off_goes_to_the_control_with_focus(make, card):
    plugin, _host, _ = make([_b("a", "Z", "transforms.zoom", {"op": "up", "amount": 10})])
    card.slider.setEnabled(False)
    card.show()
    QTest.qWaitForWindowActive(card)
    card.entry.setFocus()
    QTest.keyClick(card.entry, Qt.Key.Key_Z, Qt.KeyboardModifier.ShiftModifier)  # Shift+Z isn't bound
    card.mute.setFocus()
    event = QKeyEvent(QEvent.Type.KeyPress, Qt.Key.Key_Z, Qt.KeyboardModifier.NoModifier, "z")
    assert not plugin._filter.eventFilter(card.mute, event)
    card.slider.setEnabled(True)
    assert plugin._filter.eventFilter(card.mute, event)
    assert card.slider.value() == 10


def test_controls_that_come_later_still_get_the_menu(qapp):
    from telescope.shortcuts import choice_action
    lenses = []
    host = _Host([choice_action("camera.lens", "Lens", "Camera", buttons=lambda: lenses)])
    plugin = ShortcutsPlugin(backend_factory=lambda: None)
    plugin.setup(host, None)
    plugin.start()
    shown = []
    plugin.control_menu = lambda w, a, pos: shown.append((w, a))
    try:
        btn = QPushButton("Wide")
        lenses.append(btn)
        from PyQt6.QtGui import QContextMenuEvent
        from PyQt6.QtCore import QPoint
        event = QContextMenuEvent(QContextMenuEvent.Reason.Mouse, QPoint(1, 1))
        assert plugin._filter.eventFilter(btn, event)
        assert shown == [(btn, "camera.lens")]
        assert not plugin._filter.eventFilter(QPushButton("other"), event)
    finally:
        plugin.shutdown()


def test_f_keys_still_work_while_a_text_box_has_focus(make, card):
    plugin, _host, _ = make([_b("a", "F8", "camera.torch"), _b("b", "Z", "transforms.zoom")])
    card.show()
    QTest.qWaitForWindowActive(card)
    card.entry.setFocus()
    QTest.keyClick(card.entry, Qt.Key.Key_F8)
    assert card.torch.isChecked()
    QTest.keyClick(card.entry, Qt.Key.Key_Z)
    assert card.slider.value() == 0 and card.entry.text() == "z"


def test_an_ignored_key_runs_once_not_at_every_parent(make, card):
    plugin, _host, _ = make([_b("a", "F8", "camera.torch")])
    card.torch.setEnabled(False)
    calls = []
    real = plugin.press
    plugin.press = lambda *a, **k: (calls.append(a), real(*a, **k))[1]
    card.show()
    QTest.qWaitForWindowActive(card)
    card.mute.setFocus()
    QTest.keyClick(card.mute, Qt.Key.Key_F8)
    assert len(calls) == 1


def test_global_presses_are_ignored_while_the_editor_records_a_key(make, card):
    plugin, _host, backend = make([_b("a", "F9", "camera.torch", glob=True)])
    suspended = []
    backend.suspend = suspended.append
    from PyQt6.QtCore import QTimer

    def press_then_close():
        backend.pressed.emit("a")
        QApplication.activeModalWidget().reject()
    QTimer.singleShot(0, press_then_close)
    assert plugin.add_binding("camera.torch") is None
    assert not card.torch.isChecked()
    assert suspended == [True, False]
    backend.pressed.emit("a")
    assert card.torch.isChecked()


def test_desktop_settings_button_asks_again_for_unconfirmed_ones(make):
    plugin, _host, backend = make([_b("a", "F9", "camera.torch", glob=True)])
    backend.system_picks_keys = True
    backend.statuses["a"] = HotkeyStatus(False, "Not confirmed in the desktop's dialog")
    backend.configure = lambda: False
    backend.can_rebind = lambda: True
    before = len(backend.applied)
    assert plugin.open_system_settings()
    assert len(backend.applied) == before + 1


def test_desktop_settings_button_opens_plasmas_page_when_the_portal_cant_help(make, monkeypatch):
    plugin, _host, backend = make([_b("a", "F9", "camera.torch", glob=True)])
    backend.system_picks_keys = True
    launched = []
    monkeypatch.setattr(shortcuts_module, "IS_LINUX", True)
    monkeypatch.setattr(shortcuts_module.subprocess, "Popen", lambda cmd, **_k: launched.append(cmd))
    before = len(backend.applied)
    assert plugin.open_system_settings()
    assert launched == [["systemsettings", "kcm_keys"]]
    assert len(backend.applied) == before


def test_editor_warns_about_typing_keys_only(make):
    plugin, _host, _ = make()
    editor = BindingEditor(plugin, {"id": "n", "keys": "", "action": "camera.torch", "params": {"mode": "hold"},
                                    "global": True})
    try:
        editor._keys_edit.setKeySequence(QKeySequence("F5"))
        assert editor._problem.isHidden()
        editor._keys_edit.setKeySequence(QKeySequence("Q"))
        assert "stop typing" in editor._problem.text()
        assert not editor._copy_btn.isEnabled()  # a command can't hold a key
        mode = editor._param_widgets["mode"][1]
        mode.setCurrentIndex(mode.findData("toggle"))
        assert editor._copy_btn.isEnabled()
    finally:
        editor.deleteLater()


def test_hold_comes_back_by_scan_code_when_the_key_changes_on_release(make, card):
    plugin, _host, _ = make([_b("a", "Ctrl+Shift+1", "microphone.mute", {"mode": "hold"})])
    plugin.press("a", "app", scan=10)
    assert card.mute.isChecked()
    plugin.release_app_keys(int(Qt.Key.Key_Exclam), 10)
    assert not card.mute.isChecked()


@pytest.mark.parametrize("params, says", [
    ({"op": "up", "amount": "nan"}, "has to be a number"),
    ({"op": "up", "amount": "inf"}, "has to be a number"),
])
def test_remote_numbers_must_be_finite(make, params, says):
    plugin, _host, _ = make()
    assert says in plugin.handle_remote({"action": "transforms.zoom", "params": params})["text"]


def test_showing_the_window_from_the_background_doesnt_notify(make, monkeypatch):
    monkeypatch.setattr(QApplication, "activeWindow", staticmethod(lambda: None))
    _plugin, host, backend = make([_b("a", "Ctrl+Alt+H", "window.show_hide", glob=True)], notify=True)
    backend.pressed.emit("a")  # shown: the window itself is the answer
    backend.released.emit("a")
    backend.pressed.emit("a")
    assert host.notes == [("Telescope", "Telescope: hidden", False)]


def test_a_right_click_that_reaches_the_window_finds_the_later_control(qapp):
    from telescope.shortcuts import choice_action
    from PyQt6.QtGui import QContextMenuEvent
    from PyQt6.QtCore import QPoint
    window = QWidget()
    lay = QVBoxLayout(window)
    btn = QPushButton("Wide")
    lay.addWidget(btn)
    window.resize(200, 100)
    window.show()
    host = _Host([choice_action("camera.lens", "Lens", "Camera", buttons=lambda: [btn])])
    plugin = ShortcutsPlugin(backend_factory=lambda: None)
    plugin.setup(host, None)
    plugin.start()
    shown = []
    plugin.control_menu = lambda w, a, pos: shown.append((w, a, pos))
    try:
        at = btn.mapTo(window, QPoint(2, 3))
        assert plugin._filter.eventFilter(window, QContextMenuEvent(QContextMenuEvent.Reason.Mouse, at))
        assert shown == [(btn, "camera.lens", QPoint(2, 3))]
    finally:
        plugin.shutdown()
        window.close()


def test_the_key_filter_follows_focus_instead_of_watching_the_whole_app(make, card):
    # An app-wide Python filter sees objects Qt is tearing down, and PyQt can crash wrapping them
    plugin, _host, _ = make([_b("a", "Ctrl+T", "camera.torch")])
    card.show()
    card.activateWindow()
    QTest.qWaitForWindowActive(card)
    card.mute.setFocus()
    assert any(w is card.mute for w in plugin._filter._watched)
    card.entry.setFocus()
    assert any(w is card.entry for w in plugin._filter._watched)
    assert not any(w is card.mute for w in plugin._filter._watched)
    assert not any(w is QApplication.instance() for w in plugin._filter._watched)
    QTest.keyClick(card.entry, Qt.Key.Key_T, Qt.KeyboardModifier.ControlModifier)
    assert card.torch.isChecked()


def test_keys_reach_a_window_where_nothing_takes_focus(make, card):
    plugin, _host, _ = make([_b("a", "Ctrl+T", "camera.torch")])
    bare = QWidget()
    bare.show()
    bare.activateWindow()
    QTest.qWaitForWindowActive(bare)
    QTest.qWait(10)  # it looks once Qt has finished making the window active
    try:
        assert any(w is bare for w in plugin._filter._watched)
        QTest.keyClick(bare, Qt.Key.Key_T, Qt.KeyboardModifier.ControlModifier)
        assert card.torch.isChecked()
    finally:
        bare.close()
