"""Layout rule: widths come from the layout and text has to fit inside them (see widgets/common.py)."""

import pytest
from PyQt6.QtWidgets import QPushButton

import telescope.plugins.connection as connection_module
import telescope.plugins.setup as setup_plugin_module
from telescope.ip_utils import PairingAddress
from telescope.theme import apply_theme

# QSS horizontal padding + border of the widest button style, both sides.
_BUTTON_CHROME = 2 * 14 + 2


def _clipped_buttons(root):
    clipped = []
    for btn in root.findChildren(QPushButton):
        if btn.minimumWidth() != btn.maximumWidth() or not btn.text():
            continue  # only buttons whose width the layout fixed
        btn.ensurePolished()
        icon = btn.iconSize().width() + 4 if not btn.icon().isNull() else 0
        need = btn.fontMetrics().horizontalAdvance(btn.text().replace("&&", "&")) + icon + _BUTTON_CHROME
        if need > btn.width():
            clipped.append((btn.text(), need, btn.width()))
    return clipped


@pytest.mark.parametrize("linux", [True, False])
def test_setup_dialog_button_text_fits(qapp, monkeypatch, linux):
    apply_theme(qapp)
    monkeypatch.setattr(setup_plugin_module, "IS_LINUX", linux)
    monkeypatch.setattr(setup_plugin_module, "v4l2_devices_ready", lambda: True)
    monkeypatch.setattr(setup_plugin_module, "v4l2_persist_status",
                        lambda: {"modprobe_conf": False, "modules_load_conf": False})
    dialog = setup_plugin_module.SetupDialog()
    if not linux:
        dialog._on_win_checks(False, True)
    dialog._advanced_toggle.setChecked(True)
    dialog.show()
    qapp.processEvents()
    assert _clipped_buttons(dialog) == []
    dialog.hide()


def test_pairing_and_device_dialog_button_text_fits(qapp, monkeypatch):
    apply_theme(qapp)
    monkeypatch.setattr(connection_module, "adb_reverse", lambda *a, **k: (True, ""))
    monkeypatch.setattr(connection_module, "adb_unreverse", lambda *a, **k: None)
    monkeypatch.setattr(connection_module.ip_utils, "get_pairing_addresses",
                        lambda: [PairingAddress("192.168.1.2", "wlan0", "lan")])
    dialogs = [
        connection_module._PairingDialog(None, lambda *a: None, usb_serial="s"),
        connection_module._DeviceManagerDialog(None, [{"name": "P", "ips": ["1.2.3.4"]}],
                                               lambda: None, lambda *a: None, lambda *a: None),
        connection_module._DeviceDialog(None, [], {"name": "P", "ips": ["1.2.3.4"]}),
    ]
    for dialog in dialogs:
        dialog.show()
        qapp.processEvents()
        assert _clipped_buttons(dialog) == [], type(dialog).__name__
        dialog.close()
