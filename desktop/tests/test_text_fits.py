"""Layout rule: widths come from the layout and text has to fit inside them (see widgets/common.py)."""

import pytest
from PyQt6.QtWidgets import QPushButton

import telescope.plugins.connection as connection_module
import telescope.pairing as pairing_module
import telescope.plugins.setup as setup_plugin_module
from telescope.ip_utils import PairingAddress
from telescope.phones import Phone
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
    dialog = setup_plugin_module.AdvancedDialog()
    if not linux:
        dialog._on_win_checks(False, True)
    dialog.show()
    qapp.processEvents()
    assert _clipped_buttons(dialog) == []
    dialog.hide()


class _FakePhonesPlugin:
    computer_name = "Desk"
    phones = [Phone("id-1", "Pixel", "tok")]

    def phone(self, pid):
        return self.phones[0] if pid == "id-1" else None

    def open_add_phone(self):
        pass


def test_add_phone_and_phones_dialog_button_text_fits(qapp, monkeypatch):
    apply_theme(qapp)
    monkeypatch.setattr(connection_module, "adb_available", lambda: False)
    monkeypatch.setattr(pairing_module.ip_utils, "get_pairing_addresses",
                        lambda: [PairingAddress("192.168.1.2", "wlan0", "lan")])
    dialogs = [
        connection_module.AddPhoneDialog(None, "cid", "Desk", lambda *a: None),
        connection_module.PhonesDialog(_FakePhonesPlugin()),
    ]
    for dialog in dialogs:
        dialog.show()
        qapp.processEvents()
        assert _clipped_buttons(dialog) == [], type(dialog).__name__
        dialog.close()
