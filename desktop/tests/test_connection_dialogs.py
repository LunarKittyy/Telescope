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
        on_add=lambda device: events.append(("add", device)),
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


def test_device_manager_finish_add_and_edit_mutate_shared_list(qapp):
    dialog, devices, events, _parent = _manager(qapp, [])
    add = SimpleNamespace(result_device=lambda: {"name": "A", "ips": ["1.2.3.4"]})
    dialog._finish_add(add)
    assert devices == [{"name": "A", "ips": ["1.2.3.4"]}]
    assert events == [("add", devices[0])]

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


def test_device_manager_opens_add_then_replaces_with_edit_dialog(qapp):
    dialog, _devices, _events, _parent = _manager(qapp)
    dialog._on_add()
    first = dialog._active_dlg
    assert first is not None
    assert first.isVisible()

    dialog._list.setCurrentRow(0)
    dialog._on_edit()
    second = dialog._active_dlg
    assert second is not first
    assert second.windowTitle() == "Edit Device"
    second.close()


def test_qr_widget_builds_matrix_and_renders(qapp):
    widget = _QRCodeWidget('{"port":8765}')
    assert len(widget._matrix) > 0
    assert widget.width() == len(widget._matrix) * 8
    image = widget.grab().toImage()
    assert not image.isNull()


def test_pairing_dialog_reports_no_network_interfaces(monkeypatch, qapp):
    dialog = _PairingDialog(None, lambda *_args: None)
    monkeypatch.setattr(connection, "_get_local_ips", lambda: [])
    dialog._start_server()
    assert dialog._server is None
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


def test_pairing_start_is_idempotent_when_server_already_exists(qapp):
    dialog = _PairingDialog(None, lambda *_args: None)
    dialog._server = object()
    dialog._start_server()
    assert dialog._server is not None
