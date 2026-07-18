from types import SimpleNamespace

from PyQt6.QtWidgets import QMessageBox, QWidget

import telescope.plugins.connection as connection
from telescope.plugins.connection import (
    _DeviceManagerDialog,
    _PairingDialog,
    _QRCodeWidget,
)


def _manager(qapp, devices=None):
    events = []
    parent = QWidget()
    devices = devices if devices is not None else [
        {"name": "Phone", "ips": ["10.0.0.1", "100.64.0.1", "192.168.1.2"]}
    ]
    dialog = _DeviceManagerDialog(
        parent,
        devices,
        on_add=lambda: events.append(("add",)),
        on_edit=lambda old, new: events.append(("edit", old, new)),
        on_remove=lambda name: events.append(("remove", name)),
    )
    return dialog, devices, events, parent


def test_device_manager_renders_selection_and_truncated_ips(qapp):
    dialog, _devices, _events, _parent = _manager(qapp)
    assert dialog._list.count() == 1
    assert dialog._list.item(0).text() == "Phone  -  10.0.0.1, 100.64.0.1..."
    assert not dialog._edit_btn.isEnabled()
    assert not dialog._remove_btn.isEnabled()

    dialog._list.setCurrentRow(0)
    assert dialog._edit_btn.isEnabled()
    assert dialog._remove_btn.isEnabled()
    dialog._on_selection(-1)
    assert not dialog._edit_btn.isEnabled()


def test_device_manager_add_button_starts_pairing_flow(qapp):
    dialog, _devices, events, _parent = _manager(qapp)
    dialog._on_add()
    assert events == [("add",)]
    # Nothing local was mutated - a device only appears once pairing reports
    # back through _on_device_paired(), outside this dialog entirely.
    assert dialog._active_dlg is None


def test_device_manager_finish_edit_updates_shared_list(qapp):
    dialog, devices, events, _parent = _manager(qapp, [{"name": "A", "ips": ["1.2.3.4"]}])

    edit = SimpleNamespace(result_device=lambda: {"name": "B", "ips": ["4.3.2.1"]})
    dialog._finish_edit(0, edit)
    assert devices == [{"name": "B", "ips": ["4.3.2.1"]}]
    assert events[-1] == ("edit", "A", devices[0])
    assert dialog._list.item(0).text().startswith("B  -")


def test_device_manager_remove_cancel_and_confirm(monkeypatch, qapp):
    dialog, devices, events, _parent = _manager(qapp)
    dialog._list.setCurrentRow(0)
    monkeypatch.setattr(
        connection.QMessageBox,
        "question",
        lambda *_args: QMessageBox.StandardButton.No,
    )
    dialog._on_remove()
    assert len(devices) == 1

    monkeypatch.setattr(
        connection.QMessageBox,
        "question",
        lambda *_args: QMessageBox.StandardButton.Yes,
    )
    dialog._on_remove()
    assert devices == []
    assert events == [("remove", "Phone")]


def test_device_manager_ignores_edit_and_remove_without_selection(qapp):
    dialog, devices, events, _parent = _manager(qapp)
    dialog._list.setCurrentRow(-1)
    dialog._on_edit()
    dialog._on_remove()
    assert len(devices) == 1
    assert events == []


def test_device_manager_edit_opens_dialog_and_replaces_a_prior_one(qapp):
    dialog, devices, _events, _parent = _manager(qapp, [
        {"name": "A", "ips": ["1.2.3.4"]}, {"name": "B", "ips": ["5.6.7.8"]},
    ])
    dialog._list.setCurrentRow(0)
    dialog._on_edit()
    first = dialog._active_dlg
    assert first is not None
    assert first.isVisible()
    assert first.windowTitle() == "Edit Device"

    dialog._list.setCurrentRow(1)
    dialog._on_edit()
    second = dialog._active_dlg
    assert second is not first
    assert second.windowTitle() == "Edit Device"
    second.close()


def test_qr_widget_builds_matrix_and_renders(qapp):
    widget = _QRCodeWidget('{"port":8765}')
    assert len(widget._matrix) > 0
    assert widget.width() == len(widget._matrix) * 8 + widget._QUIET_ZONE_PX * 2
    image = widget.grab().toImage()
    assert not image.isNull()


def test_pairing_dialog_reserves_width_for_a_qr_code(qapp):
    dialog = _PairingDialog(None, lambda *_args: None)
    assert dialog.minimumWidth() >= 420


def test_pairing_dialog_reports_no_network_interfaces(monkeypatch, qapp):
    import telescope.pairing as pairing_module

    dialog = _PairingDialog(None, lambda *_args: None)
    monkeypatch.setattr(pairing_module.ip_utils, "get_local_ips", lambda: [])
    dialog._start_server()
    assert dialog._pairing_server is None
    assert dialog._status_lbl.objectName() == "status_err"
    assert dialog._status_lbl.text() == "No network interfaces found."


def test_pairing_dialog_success_ui_and_callback(qapp):
    paired = []
    dialog = _PairingDialog(None, lambda name, ips, token: paired.append((name, ips, token)))
    dialog._on_paired_signal("Phone", ["10.0.0.1"], "tok-123")
    assert paired == [("Phone", ["10.0.0.1"], "tok-123")]
    assert dialog._status_lbl.text() == ""
    assert dialog._hint_lbl.isHidden()
    labels = [dialog._qr_container.itemAt(i).widget()
              for i in range(dialog._qr_container.count())
              if dialog._qr_container.itemAt(i).widget()]
    assert any('"Phone" added.' in label.text() for label in labels)


def test_pairing_dialog_renders_qr_after_start(qapp):
    dialog = _PairingDialog(None, lambda *_args: None)
    dialog._start_server()
    try:
        assert dialog._pairing_server is not None
        widgets = [dialog._qr_container.itemAt(i).widget()
                   for i in range(dialog._qr_container.count())
                   if dialog._qr_container.itemAt(i).widget()]
        assert any(isinstance(w, _QRCodeWidget) for w in widgets)
        assert dialog._status_lbl.text() == "Scan with the Telescope app on your phone."
    finally:
        dialog._stop_server()


def test_pairing_dialog_usb_mode_reverses_port_and_shows_pair_button(monkeypatch, qapp):
    calls = []
    monkeypatch.setattr(connection, "adb_reverse", lambda port, serial=None: calls.append(("reverse", port, serial)) or (True, "ok"))
    monkeypatch.setattr(connection, "adb_unreverse", lambda port, serial=None: calls.append(("unreverse", port, serial)))
    broadcasts = []
    monkeypatch.setattr(
        connection, "adb_broadcast_pair",
        lambda payload_b64, serial=None: broadcasts.append((payload_b64, serial)) or (True, "Broadcast sent"),
    )

    dialog = _PairingDialog(None, lambda *_args: None, usb_serial="phone-1")
    dialog._start_server()
    try:
        assert dialog._pairing_server is not None
        assert calls[0][0] == "reverse"
        assert calls[0][2] == "phone-1"
        assert dialog._reversed_port == calls[0][1]
        # Reaching the phone is a deliberate, re-triggerable click, not
        # something that fires the instant the dialog opens.
        assert broadcasts == []
        assert dialog._pair_btn is not None
        assert dialog._status_lbl.text() == "Ready to pair."
    finally:
        dialog._stop_server()

    assert calls[-1] == ("unreverse", calls[0][1], "phone-1")
    assert dialog._reversed_port is None


def test_pairing_dialog_usb_mode_reports_adb_reverse_failure(monkeypatch, qapp):
    monkeypatch.setattr(connection, "adb_reverse", lambda port, serial=None: (False, "device offline"))

    dialog = _PairingDialog(None, lambda *_args: None, usb_serial="phone-1")
    dialog._start_server()

    assert dialog._pairing_server is None
    assert dialog._status_lbl.objectName() == "status_err"
    assert "device offline" in dialog._status_lbl.text()


def test_pairing_dialog_pair_button_sends_broadcast_and_awaits_response(monkeypatch, qapp):
    import base64

    monkeypatch.setattr(connection, "adb_reverse", lambda port, serial=None: (True, "ok"))
    monkeypatch.setattr(connection, "adb_unreverse", lambda port, serial=None: None)
    broadcasts = []
    monkeypatch.setattr(
        connection, "adb_broadcast_pair",
        lambda payload_b64, serial=None: broadcasts.append((payload_b64, serial)) or (True, "Broadcast sent"),
    )

    dialog = _PairingDialog(None, lambda *_args: None, usb_serial="phone-1")
    dialog._start_server()
    try:
        dialog._send_pair_broadcast()
        assert len(broadcasts) == 1
        payload_b64, serial = broadcasts[0]
        assert serial == "phone-1"
        assert base64.b64decode(payload_b64).decode() == dialog._pairing_server.offer.payload
        assert not dialog._pair_btn.isEnabled()
        assert "waiting for the phone to respond" in dialog._status_lbl.text()
        assert dialog._pair_timeout is not None
    finally:
        dialog._stop_server()


def test_pairing_dialog_pair_button_reports_broadcast_failure_and_reenables(monkeypatch, qapp):
    monkeypatch.setattr(connection, "adb_reverse", lambda port, serial=None: (True, "ok"))
    monkeypatch.setattr(connection, "adb_unreverse", lambda port, serial=None: None)
    monkeypatch.setattr(
        connection, "adb_broadcast_pair",
        lambda payload_b64, serial=None: (False, "device offline"),
    )

    dialog = _PairingDialog(None, lambda *_args: None, usb_serial="phone-1")
    dialog._start_server()
    try:
        dialog._send_pair_broadcast()
        assert dialog._status_lbl.objectName() == "status_err"
        assert "Broadcast failed" in dialog._status_lbl.text()
        assert dialog._pair_btn.isEnabled()
        assert dialog._pair_timeout is None
    finally:
        dialog._stop_server()


def test_pairing_dialog_pair_timeout_shows_message_and_reenables_button(monkeypatch, qapp):
    monkeypatch.setattr(connection, "adb_reverse", lambda port, serial=None: (True, "ok"))
    monkeypatch.setattr(connection, "adb_unreverse", lambda port, serial=None: None)
    monkeypatch.setattr(connection, "adb_broadcast_pair", lambda payload_b64, serial=None: (True, "Broadcast sent"))

    dialog = _PairingDialog(None, lambda *_args: None, usb_serial="phone-1")
    dialog._start_server()
    try:
        dialog._send_pair_broadcast()
        dialog._on_pair_timeout()
        assert dialog._status_lbl.objectName() == "status_err"
        assert "No response after 8s" in dialog._status_lbl.text()
        assert dialog._pair_btn.isEnabled()
        assert dialog._pair_timeout is None
    finally:
        dialog._stop_server()


def test_pairing_dialog_success_cancels_pending_timeout(monkeypatch, qapp):
    monkeypatch.setattr(connection, "adb_reverse", lambda port, serial=None: (True, "ok"))
    monkeypatch.setattr(connection, "adb_unreverse", lambda port, serial=None: None)
    monkeypatch.setattr(connection, "adb_broadcast_pair", lambda payload_b64, serial=None: (True, "Broadcast sent"))

    dialog = _PairingDialog(None, lambda *_args: None, usb_serial="phone-1")
    dialog._start_server()
    try:
        dialog._send_pair_broadcast()
        assert dialog._pair_timeout is not None
        dialog._on_paired_signal("Phone", ["10.0.0.1"], "tok-123")
        assert dialog._pair_timeout is None
    finally:
        dialog._stop_server()


def test_pairing_start_is_idempotent_when_server_already_exists(qapp):
    dialog = _PairingDialog(None, lambda *_args: None)
    sentinel = object()
    dialog._pairing_server = sentinel
    dialog._start_server()
    assert dialog._pairing_server is sentinel
