from pathlib import Path

import pytest
from PyQt6.QtGui import QShowEvent
from PyQt6.QtWidgets import QDialog, QFileDialog, QWidget

import telescope.plugins.setup as setup_mod
from telescope.plugin import EventBus
from telescope.plugins.setup import SetupDialog, SetupPlugin, _GuideDialog


class _ImmediateThread:
    def __init__(self, target, daemon=False):
        self.target = target
        self.daemon = daemon

    def start(self):
        self.target()


class _Host(QWidget):
    def __init__(self):
        super().__init__()
        self.saves = 0
        self.restarts = []

    def _schedule_save(self):
        self.saves += 1

    def restart_vcam_canvas(self, w, h, on_done=None):
        self.restarts.append((w, h))
        if on_done:
            on_done(True, "ok")


def test_guide_dialog_contains_documentation_and_close_button(qapp):
    dialog = _GuideDialog()
    browsers = dialog.findChildren(setup_mod.QTextBrowser)
    assert len(browsers) == 1
    assert "Quick Start" in browsers[0].toPlainText()
    close = next(button for button in dialog.findChildren(setup_mod.QPushButton)
                 if button.text() == "Close")
    close.click()
    assert dialog.result() == QDialog.DialogCode.Accepted


def test_dialog_preset_visibility_and_apply_callback(qapp):
    applied = []
    dialog = SetupDialog(on_apply_canvas=lambda w, h: applied.append((w, h)))
    dialog.show()
    dialog.set_canvas_preset("Custom...", 1000, 700)
    assert not dialog._custom_widget.isHidden()
    dialog._apply_canvas()
    assert applied == [(1000, 700)]
    assert not dialog._canvas_apply_btn.isEnabled()
    assert dialog._canvas_status_lbl.isVisible()
    dialog.hide()


@pytest.mark.parametrize(
    "devices,module,expected_text,expected_name",
    [
        (True, False, "Ready:", "status_ok"),
        (False, True, "another config", "status_warn"),
        (False, False, "Not loaded", "status_err"),
    ],
)
def test_v4l_status_states(monkeypatch, qapp, devices, module, expected_text, expected_name):
    dialog = SetupDialog()
    monkeypatch.setattr(setup_mod, "v4l2_devices_ready", lambda: devices)
    monkeypatch.setattr(setup_mod, "v4l2_module_loaded", lambda: module)
    dialog._v4l_check()
    assert expected_text in dialog._v4l_lbl.text()
    assert dialog._v4l_lbl.objectName() == expected_name


def test_v4l_async_load_unload_and_results(monkeypatch, qapp):
    dialog = SetupDialog()
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    monkeypatch.setattr(setup_mod, "v4l2_load", lambda: (True, "loaded"))
    monkeypatch.setattr(setup_mod, "v4l2_unload", lambda: (False, "busy"))

    dialog._v4l_load()
    assert dialog._v4l_lbl.objectName() == "status_ok"
    assert dialog._v4l_lbl.text() == "Loaded - loaded"
    dialog._v4l_unload()
    assert dialog._v4l_lbl.objectName() == "status_err"
    assert dialog._v4l_lbl.text() == "Failed - busy"


@pytest.mark.parametrize(
    "status,checked",
    [
        ({"modprobe_conf": True, "modules_load_conf": False}, True),
        ({"modprobe_conf": False, "modules_load_conf": True}, True),
        ({"modprobe_conf": False, "modules_load_conf": False}, False),
    ],
)
def test_refresh_persist_status(monkeypatch, qapp, status, checked):
    dialog = SetupDialog()
    monkeypatch.setattr(setup_mod, "v4l2_persist_status", lambda: status)
    dialog._refresh_persist_status()
    assert dialog._persist_chk.isChecked() is checked


def test_persist_toggle_runs_correct_action_and_displays_success(monkeypatch, qapp):
    dialog = SetupDialog()
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    actions = []
    monkeypatch.setattr(
        setup_mod,
        "v4l2_persist_enable",
        lambda: actions.append("enable") or (True, "enabled"),
    )
    monkeypatch.setattr(
        setup_mod,
        "v4l2_persist_disable",
        lambda: actions.append("disable") or (True, "disabled"),
    )

    dialog._on_persist_toggled(True)
    assert actions == ["enable"]
    assert dialog._persist_chk.isEnabled()
    assert dialog._persist_status_lbl.text() == "enabled"
    assert dialog._persist_status_lbl.objectName() == "status_ok"
    dialog._on_persist_toggled(False)
    assert actions == ["enable", "disable"]


def test_failed_persist_action_reverts_checkbox(qapp):
    dialog = SetupDialog()
    dialog._persist_chk.setChecked(True)
    dialog._on_persist_result(False, "denied")
    assert not dialog._persist_chk.isChecked()
    assert dialog._persist_status_lbl.objectName() == "status_err"
    assert dialog._persist_status_lbl.text() == "denied"


@pytest.fixture
def windows_dialog(monkeypatch, qapp):
    monkeypatch.setattr(setup_mod, "IS_LINUX", False)
    monkeypatch.setattr(setup_mod, "IS_WINDOWS", True)
    dialog = SetupDialog()
    return dialog


@pytest.mark.parametrize(
    "uc_ok,adb_ok,uc_text,uc_button,adb_text",
    [
        (True, True, "Ready", "Reinstall", "Ready"),
        (False, False, "Not installed", "Install", "Not found - USB mode unavailable"),
    ],
)
def test_windows_setup_status(
    monkeypatch, windows_dialog, tmp_path, uc_ok, adb_ok, uc_text, uc_button, adb_text
):
    dialog = windows_dialog
    (tmp_path / "UnityCaptureFilter64.dll").write_bytes(b"dll")
    monkeypatch.setattr(setup_mod, "unitycapture_dir", lambda: tmp_path)
    dialog._on_win_checks(uc_ok, adb_ok)
    assert dialog._uc_status_lbl.text() == uc_text
    assert dialog._uc_btn.text() == uc_button
    assert dialog._adb_status_lbl.text() == adb_text


def test_windows_status_offers_download_when_dll_missing(monkeypatch, windows_dialog, tmp_path):
    monkeypatch.setattr(setup_mod, "unitycapture_dir", lambda: tmp_path)
    windows_dialog._on_win_checks(False, True)
    assert windows_dialog._uc_btn.text() == "Download and Install"


def test_windows_background_check_emits_current_status(monkeypatch, windows_dialog):
    monkeypatch.setattr(setup_mod, "uc_is_registered", lambda: True)
    monkeypatch.setattr(setup_mod, "adb_available", lambda: False)
    windows_dialog._check_win_setup()
    assert windows_dialog._uc_status_lbl.text() == "Ready"
    assert "Not found" in windows_dialog._adb_status_lbl.text()


def test_install_unitycapture_downloads_then_registers(monkeypatch, windows_dialog, tmp_path):
    dialog = windows_dialog
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    monkeypatch.setattr(setup_mod, "unitycapture_dir", lambda: tmp_path)
    calls = []

    def download():
        calls.append("download")
        (tmp_path / "UnityCaptureFilter64.dll").write_bytes(b"dll")
        return True, "downloaded"

    monkeypatch.setattr(setup_mod, "download_unitycapture", download)
    monkeypatch.setattr(
        setup_mod,
        "register_unitycapture",
        lambda: calls.append("register") or (True, "installed"),
    )
    dialog._install_uc()
    assert calls == ["download", "register"]
    assert dialog._uc_status_lbl.text() == "Ready"
    assert dialog._uc_btn.text() == "Reinstall"


def test_install_unitycapture_stops_after_download_error(monkeypatch, windows_dialog, tmp_path):
    dialog = windows_dialog
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    monkeypatch.setattr(setup_mod, "unitycapture_dir", lambda: tmp_path)
    monkeypatch.setattr(setup_mod, "download_unitycapture", lambda: (False, "offline"))
    monkeypatch.setattr(
        setup_mod,
        "register_unitycapture",
        lambda: (_ for _ in ()).throw(AssertionError("must not register")),
    )
    dialog._install_uc()
    assert dialog._uc_status_lbl.text() == "Failed: offline"
    assert dialog._uc_btn.text() == "Retry"


def test_install_unitycapture_skips_download_when_dll_exists(monkeypatch, windows_dialog, tmp_path):
    (tmp_path / "UnityCaptureFilter64.dll").write_bytes(b"dll")
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    monkeypatch.setattr(setup_mod, "unitycapture_dir", lambda: tmp_path)
    monkeypatch.setattr(
        setup_mod,
        "download_unitycapture",
        lambda: (_ for _ in ()).throw(AssertionError("must not download")),
    )
    monkeypatch.setattr(setup_mod, "register_unitycapture", lambda: (False, "denied"))
    windows_dialog._install_uc()
    assert windows_dialog._uc_status_lbl.text() == "Failed: denied"


def test_apk_install_rejects_missing_adb_or_device(monkeypatch, qapp, tmp_path):
    dialog = SetupDialog()
    monkeypatch.setattr(setup_mod, "adb_available", lambda: False)
    dialog._install_apk()
    assert "adb not found" in dialog._apk_status_lbl.text()

    apk = tmp_path / "Telescope.apk"
    apk.write_bytes(b"apk")
    monkeypatch.setattr(setup_mod, "adb_available", lambda: True)
    monkeypatch.setattr(setup_mod, "bundled_apk_path", lambda: apk)
    monkeypatch.setattr(setup_mod, "adb_devices", lambda: [])
    dialog._install_apk()
    assert "No authorized" in dialog._apk_status_lbl.text()


def test_apk_install_uses_selected_device_and_reports_success(monkeypatch, qapp, tmp_path):
    dialog = SetupDialog()
    apk = tmp_path / "Telescope.apk"
    apk.write_bytes(b"apk")
    monkeypatch.setattr(setup_mod, "adb_available", lambda: True)
    monkeypatch.setattr(setup_mod, "bundled_apk_path", lambda: apk)
    monkeypatch.setattr(setup_mod, "adb_devices", lambda: ["a", "b"])
    monkeypatch.setattr(setup_mod, "adb_exe", lambda: "adb")
    monkeypatch.setattr(setup_mod.QInputDialog, "getItem", lambda *_args: ("b", True))
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    calls = []
    monkeypatch.setattr(
        setup_mod,
        "_run",
        lambda cmd, timeout: calls.append((cmd, timeout)) or (0, "Success\n", ""),
    )

    dialog._install_apk()

    assert calls == [(["adb", "-s", "b", "install", "-r", str(apk)], 60)]
    assert dialog._apk_status_lbl.text() == "Installed successfully"
    assert dialog._apk_status_lbl.objectName() == "status_ok"


def test_apk_install_cancel_and_failure_detail(monkeypatch, qapp, tmp_path):
    dialog = SetupDialog()
    apk = tmp_path / "Telescope.apk"
    apk.write_bytes(b"apk")
    monkeypatch.setattr(setup_mod, "adb_available", lambda: True)
    monkeypatch.setattr(setup_mod, "bundled_apk_path", lambda: apk)
    monkeypatch.setattr(setup_mod, "adb_devices", lambda: ["a", "b"])
    monkeypatch.setattr(setup_mod.QInputDialog, "getItem", lambda *_args: ("a", False))
    dialog._install_apk()
    assert dialog._apk_btn.isEnabled()

    monkeypatch.setattr(setup_mod, "adb_devices", lambda: ["a"])
    monkeypatch.setattr(setup_mod, "adb_exe", lambda: "adb")
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    monkeypatch.setattr(
        setup_mod,
        "_run",
        lambda *_args, **_kwargs: (1, "line one\n", "Failure [bad apk]\n"),
    )
    dialog._install_apk()
    assert dialog._apk_status_lbl.text() == "Failure [bad apk]"
    assert dialog._apk_status_lbl.objectName() == "status_err"


def test_apk_picker_cancel_and_selected_file(monkeypatch, qapp, tmp_path):
    dialog = SetupDialog()
    initial_status = dialog._apk_status_lbl.text()
    monkeypatch.setattr(setup_mod, "adb_available", lambda: True)
    monkeypatch.setattr(setup_mod, "bundled_apk_path", lambda: None)
    monkeypatch.setattr(
        QFileDialog,
        "getOpenFileName",
        lambda *_args: ("", ""),
    )
    dialog._install_apk()
    assert dialog._apk_status_lbl.text() == initial_status

    chosen = tmp_path / "chosen.apk"
    monkeypatch.setattr(
        QFileDialog,
        "getOpenFileName",
        lambda *_args: (str(chosen), "Android Package (*.apk)"),
    )
    monkeypatch.setattr(setup_mod, "adb_devices", lambda: ["phone"])
    monkeypatch.setattr(setup_mod, "adb_exe", lambda: "adb")
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    monkeypatch.setattr(setup_mod, "_run", lambda *_args, **_kwargs: (1, "", ""))
    dialog._install_apk()
    assert dialog._apk_status_lbl.text() == "unknown error"


def test_setup_plugin_opens_reuses_dialogs_and_syncs_config(monkeypatch, qapp):
    host = _Host()
    plugin = SetupPlugin()
    plugin.setup(host, EventBus())
    panel = plugin.create_panel()
    plugin.set_config({
        "canvas_preset": "Custom...",
        "custom_canvas_w": 900,
        "custom_canvas_h": 600,
    })

    plugin._open()
    first = plugin._dlg
    assert first.get_canvas_preset_label() == "Custom..."
    assert first._custom_w.value() == 900
    plugin._open()
    assert plugin._dlg is first
    first.hide()

    plugin._open_guide()
    guide = plugin._guide_dlg
    plugin._open_guide()
    assert plugin._guide_dlg is guide
    guide.hide()


def test_show_event_refreshes_platform_specific_status(monkeypatch, qapp):
    dialog = SetupDialog()
    calls = []
    monkeypatch.setattr(dialog, "_v4l_check", lambda: calls.append("check"))
    monkeypatch.setattr(dialog, "_refresh_persist_status", lambda: calls.append("persist"))
    dialog.showEvent(QShowEvent())
    assert calls == ["check", "persist"]

    monkeypatch.setattr(setup_mod, "IS_LINUX", False)
    windows = SetupDialog()
    monkeypatch.setattr(setup_mod.threading, "Thread", _ImmediateThread)
    monkeypatch.setattr(windows, "_check_win_setup", lambda: calls.append("windows"))
    windows.showEvent(QShowEvent())
    assert calls[-1] == "windows"
