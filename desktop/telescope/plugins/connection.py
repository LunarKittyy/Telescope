"""Paired phones, how to reach them, and pairing new ones.

The user-facing model: you add phones (one dialog: scan a code, or just plug in over USB), pick which
one to use, and press Start. How it's reached is decided per connection by phones.RouteResolver:
USB whenever this phone answers over a cable, Wi-Fi otherwise, with the reason shown next to the
route and a one-click override. Tokens, ports and adb forwards never reach the UI.
"""

import base64
import contextlib
import logging
import socket
import threading
import time
import uuid
from dataclasses import dataclass
from typing import Optional

from PyQt6.QtCore import QObject, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtWidgets import (
    QDialog, QHBoxLayout, QInputDialog, QLabel, QListWidget, QListWidgetItem,
    QMessageBox, QPushButton, QVBoxLayout, QWidget,
)

from telescope import theme
from telescope.discovery import LanDiscovery
from telescope.pairing import PairingServer
from telescope.phones import (
    LOCAL_ONLY, NOT_PAIRED, READY, ROUTE_AUTO, ROUTE_USB, ROUTE_WIFI, STREAM_PORT, UNREACHABLE,
    USB_NEEDS_ATTENTION, USB_NO_ADB, USB_NO_CABLE, Phone, Resolution, Route, RouteResolver, UsbTunnels, usb_note_text,
)
from telescope.platform import (
    IS_LINUX, adb_available, adb_broadcast_pair, adb_device_states, adb_forward_auto,
    adb_reverse, adb_unforward, adb_unreverse,
)
from telescope.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV, v4l2_devices_ready, v4l2_load, v4l2_module_loaded,
)
from telescope.plugin import TelescopePlugin
from telescope.session_client import (
    PING_PORT, START_POLL_INTERVAL, START_TIMEOUT, PhoneSessionClient,
)
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, action_button, add_card_header, button_row, card_action,
    card_layout, control_row as _row, control_row_widget, create_card, create_vector_icon,
    dialog_buttons, dialog_header, dialog_layout, run_off_ui_thread, set_status_kind, set_ui_role,
    wrapped_note,
)
from telescope.widgets.qr import QRCodeWidget

logger = logging.getLogger(__name__)

_STATUS_POLL_MS = 3_000     # idle re-check of the selected phone
_USB_WATCH_MS = 5_000       # while streaming over Wi-Fi: has the phone been plugged in?
_PAIR_USB_POLL_MS = 2_000   # Add phone dialog: look for a phone on USB to pair with

# Tolerated unreachable pings while the camera starts; startup can briefly starve the phone's HTTP server.
_UNREACHABLE_STREAK_LIMIT = 3


def default_computer_name() -> str:
    name = socket.gethostname().split(".")[0].strip()
    return name or "Computer"


@dataclass(frozen=True)
class SessionTarget:
    """What worker threads need to talk to the phone, snapshotted on the GUI thread."""
    token: Optional[str]
    route: Optional[Route]


# ── Status text ───────────────────────────────────────────────────────────────

def status_line(res: Optional[Resolution]) -> tuple:
    """(status kind, short text) for the Connection card's Phone row."""
    if res is None:
        return "status_dim", "Checking…"
    return {
        READY: ("status_ok", "● Ready"),
        UNREACHABLE: ("status_warn", "○ Can't reach the phone"),
        NOT_PAIRED: ("status_err", "○ Needs pairing again"),
        LOCAL_ONLY: ("status_warn", "○ Phone accepts USB only"),
        USB_NEEDS_ATTENTION: ("status_warn", "○ USB not available"),
    }.get(res.status, ("status_dim", ""))


def problem_text(res: Resolution, phone_name: str, preference: str) -> str:
    """What's wrong and what to do about it, for the card note and for Start failures."""
    if res.status == UNREACHABLE:
        return (f"Open Telescope on {phone_name} and keep it on screen. It needs to be on the same "
                "network as this computer, or plugged in with a USB cable.")
    if res.status == NOT_PAIRED:
        return (f"{phone_name} doesn't recognise this computer anymore (it was removed on the phone, "
                "or the app was reinstalled). Click Add phone to pair it again.")
    if res.status == LOCAL_ONLY:
        return (f"Local only is on in the phone app, so {phone_name} only accepts USB. Plug it in "
                "with a cable, or turn Local only off on the phone.")
    if res.status == USB_NEEDS_ATTENTION:
        why = usb_note_text(res.usb_note) or "the phone isn't answering over USB"
        return f"The connection is set to USB, but {why}. Switch to Automatic to use Wi-Fi instead."
    return ""


def route_text(route: Optional[Route]) -> str:
    if route is None:
        return "—"
    return "USB cable" if route.kind == "usb" else f"Wi-Fi · {route.host}"


_ROUTE_CHOICES = ((ROUTE_AUTO, "Automatic"), (ROUTE_USB, "USB only"), (ROUTE_WIFI, "Wi-Fi only"))


# ── Add phone ─────────────────────────────────────────────────────────────────

class _PairSignals(QObject):
    paired = pyqtSignal(object)        # PairingResult
    usb_state = pyqtSignal(object)     # list of (serial, state) from adb


class AddPhoneDialog(QDialog):
    """One way in: scan the code over Wi-Fi, or plug the phone in and it pairs over USB by itself."""

    def __init__(self, parent, computer_id: str, computer_name: str, on_paired):
        super().__init__(parent)
        self.setWindowTitle("Add phone")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumWidth(460)
        self._on_paired = on_paired
        self._computer = (computer_id, computer_name)
        self._server: Optional[PairingServer] = None
        self._reversed: set = set()          # serials with an adb reverse to the pairing server
        self._last_broadcast: dict = {}      # serial -> monotonic time of last pairing broadcast
        self._done = False
        self._signals = _PairSignals()
        self._signals.paired.connect(self._on_paired_signal)
        self._signals.usb_state.connect(self._on_usb_state)
        self._usb_timer = QTimer(self)
        self._usb_timer.timeout.connect(self._poll_usb)
        self._build_ui()

    def _build_ui(self):
        lay = dialog_layout(self)
        self._subtitle = dialog_header(lay, "Add a phone",
                                       "Open Telescope on the phone, then use either way below.")

        self._qr_container = QVBoxLayout()
        self._qr_container.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        lay.addLayout(self._qr_container)

        self._wifi_lbl = wrapped_note("Starting…")
        self._wifi_row = control_row_widget("Wi-Fi", self._wifi_lbl, stretch=True)
        lay.addWidget(self._wifi_row)

        self._usb_lbl = wrapped_note("")
        set_status_kind(self._usb_lbl, "status_dim")
        self._usb_row = control_row_widget("USB", self._usb_lbl, stretch=True)
        lay.addWidget(self._usb_row)

        self._result_lbl = QLabel("")
        self._result_lbl.setObjectName("dialog_title")
        self._result_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._result_lbl.setVisible(False)
        lay.addWidget(self._result_lbl)

        self._close_btn = QPushButton("Cancel")
        self._close_btn.clicked.connect(self.reject)
        dialog_buttons(lay, self._close_btn)

    def showEvent(self, event):
        super().showEvent(event)
        self._start()

    def done(self, result):
        self._stop()
        super().done(result)

    def _start(self):
        if self._server is not None or self._done:
            return
        signals = self._signals
        self._server = PairingServer(
            on_paired=lambda r: signals.paired.emit(r),
            computer_id=self._computer[0], computer_name=self._computer[1],
        )
        offer = self._server.start()
        if offer.candidates:
            self._qr_container.addWidget(QRCodeWidget(offer.payload))
            self._wifi_lbl.setText("On the phone, tap Scan pairing code and point it at this code.")
        else:
            self._wifi_lbl.setText("This computer isn't on a network right now, so pair over USB.")
        if adb_available():
            self._usb_lbl.setText("Plug the phone in with a USB cable and it pairs by itself.")
            self._usb_timer.start(_PAIR_USB_POLL_MS)
            self._poll_usb()
        else:
            self._usb_lbl.setText("Needs adb, which isn't installed. Wi-Fi works without it.")

    def _stop(self):
        self._usb_timer.stop()
        server, self._server = self._server, None
        if server is not None:
            port = server.offer.port if server.offer else None
            server.stop()
            if port is not None:
                for serial in list(self._reversed):
                    threading.Thread(target=adb_unreverse, args=(port,), kwargs={"serial": serial},
                                     daemon=True).start()
        self._reversed.clear()

    def _poll_usb(self):
        signals = self._signals

        def work():
            try:
                signals.usb_state.emit(adb_device_states())
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_usb_state(self, states: list):
        if self._server is None or self._server.offer is None or self._done:
            return
        usable = [s for s, state in states if state == "device"]
        if not usable:
            if any(state == "unauthorized" for _s, state in states):
                self._set_usb("Phone plugged in. Allow USB debugging in the prompt on the phone.", "status_warn")
            else:
                self._set_usb("Plug the phone in with a USB cable and it pairs by itself.", "status_dim")
            return
        self._set_usb("Phone plugged in. Keep Telescope open on it; pairing…", "status_dim")
        offer = self._server.offer
        payload_b64 = base64.b64encode(offer.usb_payload.encode()).decode()
        now = time.monotonic()
        for serial in usable:
            # Re-sent every few seconds: the broadcast only lands while the app is on screen.
            if now - self._last_broadcast.get(serial, 0) < 4:
                continue
            self._last_broadcast[serial] = now
            first = serial not in self._reversed
            self._reversed.add(serial)

            def send(serial=serial, first=first):
                if first:
                    adb_reverse(offer.port, serial=serial)
                adb_broadcast_pair(payload_b64, serial=serial)
            threading.Thread(target=send, daemon=True).start()

    def _set_usb(self, text: str, kind: str):
        self._usb_lbl.setText(text)
        set_status_kind(self._usb_lbl, kind)

    def _on_paired_signal(self, result):
        if self._done:
            return
        self._done = True
        self._stop()
        while self._qr_container.count():
            item = self._qr_container.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        self._wifi_row.setVisible(False)
        self._usb_row.setVisible(False)
        self._subtitle.setText("It's ready to use. Pick it at the top of the window any time.")
        self._result_lbl.setText(f"Paired with {result.name}")
        self._result_lbl.setVisible(True)
        self._close_btn.setText("Done")
        set_ui_role(self._close_btn, "primary")
        self._close_btn.clicked.disconnect()
        self._close_btn.clicked.connect(self.accept)
        self._on_paired(result)


# ── Phones list ───────────────────────────────────────────────────────────────

class PhonesDialog(QDialog):
    def __init__(self, plugin: "ConnectionPlugin", parent=None):
        super().__init__(parent)
        self._plugin = plugin
        self.setWindowTitle("Your phones")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumSize(440, 360)
        lay = dialog_layout(self)
        dialog_header(lay, "Your phones",
                      "Each phone keeps its own camera settings. Removing one also unpairs it on the phone.")
        self._list = QListWidget()
        self._list.currentRowChanged.connect(self._on_selection)
        lay.addWidget(self._list, 1)
        self._add_btn = QPushButton("Add phone")
        self._rename_btn = QPushButton("Rename")
        self._remove_btn = QPushButton("Remove")
        set_ui_role(self._add_btn, "primary")
        set_ui_role(self._remove_btn, "danger")
        self._add_btn.clicked.connect(plugin.open_add_phone)
        self._rename_btn.clicked.connect(self._rename)
        self._remove_btn.clicked.connect(self._remove)
        lay.addLayout(button_row(self._add_btn, self._rename_btn, self._remove_btn))
        self._computer_lbl = ElidingLabel("")
        rename_computer = action_button("Change name", tooltip="The name your phones show for this computer")
        rename_computer.clicked.connect(self._rename_computer)
        computer_row = QHBoxLayout()
        computer_row.setContentsMargins(0, 0, 0, 0)
        computer_row.setSpacing(8)
        computer_row.addWidget(self._computer_lbl, 1)
        computer_row.addWidget(rename_computer)
        lay.addLayout(_row("This computer", computer_row, stretch=True))
        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        dialog_buttons(lay, close_btn)
        self.refresh()

    def refresh(self):
        self._computer_lbl.setText(self._plugin.computer_name)
        self._list.clear()
        for phone in self._plugin.phones:
            item = QListWidgetItem(phone.name)
            item.setData(Qt.ItemDataRole.UserRole, phone.id)
            self._list.addItem(item)
        self._on_selection(self._list.currentRow())

    def _current_id(self) -> Optional[str]:
        item = self._list.currentItem()
        return item.data(Qt.ItemDataRole.UserRole) if item else None

    def _on_selection(self, _row: int):
        ok = self._current_id() is not None
        self._rename_btn.setEnabled(ok)
        self._remove_btn.setEnabled(ok)

    def _rename(self):
        pid = self._current_id()
        phone = self._plugin.phone(pid)
        if phone is None:
            return
        name, ok = QInputDialog.getText(self, "Rename phone", "Name", text=phone.name)
        if ok and name.strip():
            self._plugin.rename_phone(pid, name.strip())
            self.refresh()

    def _rename_computer(self):
        name, ok = QInputDialog.getText(self, "Rename this computer",
                                        "Name your phones show (applies to phones you pair from now on)",
                                        text=self._plugin.computer_name)
        if ok and name.strip():
            self._plugin.set_computer_name(name.strip())
            self.refresh()

    def _remove(self):
        pid = self._current_id()
        phone = self._plugin.phone(pid)
        if phone is None:
            return
        r = QMessageBox.question(
            self, "Remove phone",
            f'Remove "{phone.name}"? This deletes its camera settings on this computer, and unpairs it '
            "on the phone too if Telescope can reach it.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if r == QMessageBox.StandardButton.Yes:
            self._plugin.forget_phone(pid)
            self.refresh()


# ── Plugin ────────────────────────────────────────────────────────────────────

class _Signals(QObject):
    resolved = pyqtSignal(int, object, object)   # check id, phone id, Resolution
    usb_available = pyqtSignal(int, bool)        # watch id, our phone answers over USB


class ConnectionPlugin(TelescopePlugin):
    name = "connection"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._phones: list = []
        self._selected_id: Optional[str] = None
        self._active_key: Optional[str] = None
        self._route_pref = ROUTE_AUTO
        self._computer_id = uuid.uuid4().hex
        self._computer_name = default_computer_name()
        self._resolution: Optional[Resolution] = None
        self._check_id = 0
        self._watch_id = 0
        self._streaming = False
        self._connected = False  # first frame arrived (EventBus.stream_connected)
        self._stream_route: Optional[Route] = None
        self._stream_forward_serial: Optional[str] = None
        self._switching = False
        self._add_dlg: Optional[AddPhoneDialog] = None
        self._phones_dlg: Optional[PhonesDialog] = None
        self._discovery = LanDiscovery()
        self._tunnels = UsbTunnels(
            forward=lambda serial, remote: adb_forward_auto(remote, serial=serial),
            unforward=lambda serial, local: adb_unforward(local, serial=serial),
        )
        self._resolver = RouteResolver(
            adb_available=adb_available, adb_device_states=adb_device_states,
            tunnels=self._tunnels, discover=self._discovery.lookup,
        )
        self._signals = _Signals()
        self._signals.resolved.connect(self._on_resolved)
        self._signals.usb_available.connect(self._on_usb_available)
        bus.stream_connected.connect(self._on_stream_connected)
        bus.add_phone_requested.connect(self.open_add_phone)

    # ── Model ─────────────────────────────────────────────────────────────

    @property
    def phones(self) -> list:
        return list(self._phones)

    def phone(self, pid: Optional[str]) -> Optional[Phone]:
        return next((p for p in self._phones if p.id == pid), None)

    def _selected_phone(self) -> Optional[Phone]:
        return self.phone(self._selected_id)

    @property
    def selected_device(self) -> Optional[str]:
        """Key the host stores per-phone settings under: the selected phone's id."""
        return self._selected_id

    @property
    def computer_name(self) -> str:
        return self._computer_name

    def set_computer_name(self, name: str):
        self._computer_name = name
        self._host.save_now()

    @property
    def resolution(self) -> Optional[Resolution]:
        return self._resolution

    # ── UI ────────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._add_btn = card_action("Add phone", "qr", "Pair a phone over Wi-Fi or USB")
        self._add_btn.clicked.connect(self.open_add_phone)
        add_card_header(lay, "Connection", "connection", action=self._add_btn)

        self._status_lbl = ElidingLabel("")
        lay.addLayout(_row("Phone", self._status_lbl, stretch=True))

        self._using_lbl = ElidingLabel("—")
        self._using_row = control_row_widget("Using", self._using_lbl, stretch=True)
        lay.addWidget(self._using_row)

        self._route_combo = NoScrollComboBox()
        for key, label in _ROUTE_CHOICES:
            self._route_combo.addItem(label, key)
        self._route_combo.setToolTip("Automatic uses USB whenever this phone answers over a cable, "
                                     "and Wi-Fi otherwise. Checked again on every connect.")
        self._route_combo.currentIndexChanged.connect(
            lambda i: self.set_route_preference(self._route_combo.itemData(i)))
        self._route_row = control_row_widget("Connect via", self._route_combo, stretch=True)
        lay.addWidget(self._route_row)

        self._note_lbl = wrapped_note("")
        self._note_row = control_row_widget("", self._note_lbl, stretch=True)
        lay.addWidget(self._note_row)

        self._switch_usb_btn = action_button("Switch to USB", "primary",
                                             "The phone was plugged in; reconnect over the cable")
        self._switch_usb_btn.clicked.connect(self._switch_to_usb)
        self._switch_usb_row = control_row_widget("", self._switch_usb_btn)
        self._switch_usb_row.setVisible(False)
        lay.addWidget(self._switch_usb_row)

        self._build_header()

        self._status_timer = QTimer(card)
        self._status_timer.timeout.connect(self._check_status)
        self._status_timer.start(_STATUS_POLL_MS)
        self._usb_watch_timer = QTimer(card)
        self._usb_watch_timer.timeout.connect(self._watch_usb)
        self._render()
        return card

    def create_header_widget(self) -> QWidget:
        return self._header_w

    def _build_header(self):
        self._header_w = QWidget()
        self._header_w.setObjectName("card_body")
        lay = QHBoxLayout(self._header_w)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        self._phone_combo = NoScrollComboBox()
        self._phone_combo.setMinimumWidth(180)
        self._phone_combo.setPlaceholderText("No phone yet")
        self._phone_combo.setToolTip("Which phone to stream from")
        self._phone_combo.currentIndexChanged.connect(self._on_combo_changed)
        lay.addWidget(self._phone_combo)
        self._manage_btn = QPushButton()
        self._manage_btn.setObjectName("icon_btn")
        self._manage_btn.setFixedSize(34, 34)
        self._manage_btn.setIcon(create_vector_icon("devices", theme.TEXT_DIM))
        self._manage_btn.setIconSize(QSize(18, 18))
        self._manage_btn.setToolTip("Your phones: add, rename, remove")
        self._manage_btn.clicked.connect(self.open_phones)
        lay.addWidget(self._manage_btn)

    def _refresh_combo(self):
        self._switching = True
        self._phone_combo.blockSignals(True)
        self._phone_combo.clear()
        for p in self._phones:
            self._phone_combo.addItem(p.name, p.id)
        self._phone_combo.setCurrentIndex(self._phone_combo.findData(self._selected_id))
        self._phone_combo.blockSignals(False)
        self._switching = False
        self._bus.phones_changed.emit(len(self._phones))

    def _render(self):
        """Card rows from the current phone + latest resolution."""
        phone = self._selected_phone()
        if phone is None:
            set_status_kind(self._status_lbl, "status_dim")
            self._status_lbl.setText("No phone yet")
            self._using_row.setVisible(False)
            self._route_row.setVisible(False)
            self._note_lbl.setText("Click Add phone to pair one. It takes a scan, or just a USB cable.")
            self._note_row.setVisible(True)
            return
        res = self._resolution
        kind, text = status_line(res)
        if self._streaming:
            kind, text = ("status_ok", "● Streaming") if self._connected else ("status_dim", "Connecting…")
        set_status_kind(self._status_lbl, kind)
        self._status_lbl.setText(text)
        self._using_row.setVisible(True)
        self._route_row.setVisible(True)
        self._using_lbl.setText(route_text(self._stream_route if self._streaming else (res.route if res else None)))
        self._route_combo.blockSignals(True)
        self._route_combo.setCurrentIndex(self._route_combo.findData(self._route_pref))
        self._route_combo.blockSignals(False)
        note = ""
        if res is not None and not self._streaming:
            note = problem_text(res, phone.name, self._route_pref)
            if (not note and res.route is not None and res.route.kind == "wifi"
                    and res.usb_note not in (None, USB_NO_CABLE, USB_NO_ADB)):
                # A cable is in but not used: exactly when "why isn't it on USB?" needs an answer.
                reason = usb_note_text(res.usb_note)
                note = f"Not using USB: {reason}."
        self._note_lbl.setText(note)
        self._note_row.setVisible(bool(note))

    # ── Status checks ─────────────────────────────────────────────────────

    def _check_status(self):
        if self._streaming:
            return
        phone = self._selected_phone()
        if phone is None:
            self._render()
            return
        self._discovery.start()
        self._check_id += 1
        self._spawn_resolve(self._check_id, Phone(**phone.to_dict()), self._route_pref)

    def _spawn_resolve(self, check_id: int, phone: Phone, preference: str):
        """Background resolve (split out so tests can run it synchronously)."""
        signals, resolver = self._signals, self._resolver

        def work():
            try:
                res = resolver.resolve(phone, preference)
            except Exception:
                logger.exception("Route resolve failed")
                res = Resolution(UNREACHABLE)
            try:
                signals.resolved.emit(check_id, phone.id, res)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_resolved(self, check_id: int, phone_id: str, res: Resolution):
        if check_id != self._check_id or phone_id != self._selected_id:
            return  # stale: a newer check, or the user switched phones meanwhile
        self._apply_resolution(res)

    def _apply_resolution(self, res: Resolution):
        self._resolution = res
        phone = self._selected_phone()
        if phone and res.route is not None and res.route.kind == "wifi" and phone.active_ip != res.route.host:
            phone.active_ip = res.route.host  # remember the address that answered (e.g. after DHCP moved it)
            if res.route.host not in phone.ips:
                phone.ips.append(res.route.host)
            self._host.schedule_save()
        self._render()

    def set_route_preference(self, preference: str):
        if preference == self._route_pref:
            return
        self._route_pref = preference
        self._resolution = None
        self._host.schedule_save()
        self._render()
        if self._streaming:
            self._host.reconnect_stream()
        else:
            self._check_status()

    # ── Stream lifecycle (called by the host) ─────────────────────────────

    def get_stream_info(self) -> tuple:
        if IS_LINUX and not self._ensure_virtual_camera():
            return None, None, False
        phone = self._selected_phone()
        if phone is None:
            QMessageBox.information(self._host, "No phone yet",
                                    "Add a phone first: click Add phone on the Connection panel.")
            return None, None, False
        res = run_off_ui_thread(self._resolver.resolve, Phone(**phone.to_dict()), self._route_pref)
        self._check_id += 1  # anything in flight is older than this
        self._apply_resolution(res)
        if res.status != READY:
            QMessageBox.warning(self._host, "Can't connect to the phone",
                                problem_text(res, phone.name, self._route_pref))
            return None, None, False
        route = res.route
        if route.kind == "usb":
            local = run_off_ui_thread(self._tunnels.acquire, route.serial, STREAM_PORT)
            if local is None:
                QMessageBox.warning(self._host, "Can't connect to the phone",
                                    "adb couldn't open a connection to the phone. Unplug it, plug it "
                                    "back in and try again.")
                return None, None, False
            self._stream_forward_serial = route.serial
            url = f"http://127.0.0.1:{local}/v1/video"
        else:
            url = f"http://{route.host}:{STREAM_PORT}/v1/video"
        self._stream_route = route
        return url, phone.token, True

    def _ensure_virtual_camera(self) -> bool:
        if v4l2_devices_ready():
            return True
        if v4l2_module_loaded():
            QMessageBox.warning(
                self._host, "Virtual camera is set up differently",
                f"v4l2loopback is already loaded, but without {V4L2_PHONE_DEV}. Another app set it up "
                "with different settings, and Telescope leaves that alone rather than break it.\n\n"
                "To hand it over to Telescope, close the other app and run:\n"
                "    sudo modprobe -r v4l2loopback\n\nThen click Start again.")
            return False
        r = QMessageBox.question(
            self._host, "Set up the virtual camera",
            "Telescope needs to load its virtual camera (v4l2loopback) first. This asks for your "
            f"password.\n\nIt creates {V4L2_PHONE_DEV} for the phone and {V4L2_OBS_DEV} for OBS.",
            QMessageBox.StandardButton.Ok | QMessageBox.StandardButton.Cancel,
            QMessageBox.StandardButton.Ok)
        if r != QMessageBox.StandardButton.Ok:
            return False
        ok, msg = run_off_ui_thread(v4l2_load)
        if not ok:
            QMessageBox.critical(self._host, "Couldn't set up the virtual camera", msg)
        return ok

    def session_target(self) -> SessionTarget:
        phone = self._selected_phone()
        route = self._stream_route or (self._resolution.route if self._resolution else None)
        return SessionTarget(phone.token if phone else None, route)

    @contextlib.contextmanager
    def session_channel(self, target: SessionTarget):
        """Yields a client for the phone's session port along target.route, or None if there's no route."""
        route = target.route
        if not target.token or route is None:
            yield None
            return
        if route.kind == "wifi":
            yield PhoneSessionClient(f"http://{route.host}:{PING_PORT}", target.token)
            return
        local = self._tunnels.acquire(route.serial, PING_PORT)
        if local is None:
            yield None
            return
        try:
            yield PhoneSessionClient(f"http://127.0.0.1:{local}", target.token)
        finally:
            self._tunnels.release(route.serial, PING_PORT)

    def ensure_phone_streaming(self, on_progress=None, target: Optional[SessionTarget] = None) -> tuple:
        """Start the phone's camera if it isn't running; blocking, worker threads only."""
        target = target or self.session_target()
        with self.session_channel(target) as client:
            if client is None:
                return False, "Lost the connection to the phone. Try again."
            ping = client.ping()
            if ping.status == "not_paired":
                return False, ("The phone doesn't recognise this computer anymore.\n\n"
                               "Click Add phone to pair it again.")
            if ping.status != "paired":
                return False, "Couldn't reach the phone. Open Telescope on it and try again."
            if ping.streaming:
                return True, ""
            if not ping.busy:
                if on_progress:
                    on_progress("Starting the phone's camera...")
                result = client.start()
                if not result.ok:
                    return False, self._start_refused_reason(result.error)
            return self._await_streaming(client, on_progress)

    @staticmethod
    def _await_streaming(client: PhoneSessionClient, on_progress=None) -> tuple:
        """Poll until the phone reports a live stream (the start request returns before the camera is up)."""
        deadline = time.monotonic() + START_TIMEOUT
        wait_start = time.monotonic()
        started = False
        unreachable_streak = 0
        while time.monotonic() < deadline:
            time.sleep(START_POLL_INTERVAL)
            ping = client.ping()
            elapsed = time.monotonic() - wait_start
            if ping.streaming:
                return True, ""
            if ping.status == "not_paired":
                return False, "Lost contact with the phone while its camera was starting."
            if ping.status != "paired":
                unreachable_streak += 1  # tolerate brief blips; bail on sustained failure
                if unreachable_streak >= _UNREACHABLE_STREAK_LIMIT:
                    return False, "Lost contact with the phone while its camera was starting."
                if on_progress:
                    on_progress(f"Phone went quiet for a moment, still waiting... ({elapsed:.0f}s)")
                continue
            unreachable_streak = 0
            if ping.busy:
                started = True
            elif started:
                return False, ("The phone's camera stopped before it finished starting.\n\n"
                               "Check the phone for a permission prompt or an error.")
            if on_progress:
                phase = "Phone's camera is opening" if started else "Waiting for the phone's camera"
                on_progress(f"{phase}... ({elapsed:.0f}s)")
        return False, ("The phone's camera didn't finish starting in time.\n\n"
                       "Try again, or start the stream on the phone.")

    def stop_phone_streaming(self, target: Optional[SessionTarget] = None):
        """Tell the phone to stop its camera (best effort); blocking, worker threads only."""
        with self.session_channel(target or self.session_target()) as client:
            if client is not None:
                client.stop()

    @staticmethod
    def _start_refused_reason(error: Optional[str]) -> str:
        return {
            "no_camera_permission": ("The phone hasn't given Telescope camera access.\n\n"
                                     "Open the app on the phone and allow the camera permission."),
            "busy": "The phone is busy starting or stopping a stream.\n\nTry again in a moment.",
            "start_refused": ("Android didn't let the camera start in the background.\n\n"
                              "Bring Telescope to the front on the phone and try again."),
            "not_paired": "The phone doesn't recognise this computer anymore.\n\nClick Add phone to pair it again.",
        }.get(error or "", f"The phone refused to start streaming ({error or 'unknown error'}).")

    def on_stream_start(self, stream_url: str, ctrl):
        if not self._streaming:  # also called after an auto-reconnect; that isn't a fresh start
            self._connected = False
        self._streaming = True
        self._switch_usb_row.setVisible(False)
        if (self._stream_route is not None and self._stream_route.kind == "wifi"
                and self._route_pref == ROUTE_AUTO):
            self._usb_watch_timer.start(_USB_WATCH_MS)
        self._render()

    def on_stream_stop(self):
        self._streaming = False
        self._usb_watch_timer.stop()
        self._watch_id += 1
        self._switch_usb_row.setVisible(False)
        if self._stream_forward_serial is not None:
            serial, self._stream_forward_serial = self._stream_forward_serial, None
            run_off_ui_thread(self._tunnels.release, serial, STREAM_PORT)
        self._stream_route = None
        self._render()
        self._check_status()

    def _on_stream_connected(self):
        self._connected = True
        self._render()

    # ── Plugged in while streaming over Wi-Fi ─────────────────────────────

    def _watch_usb(self):
        phone = self._selected_phone()
        if phone is None or not self._streaming:
            return
        self._watch_id += 1
        watch_id, signals, resolver = self._watch_id, self._signals, self._resolver
        probe = Phone(**phone.to_dict())

        def work():
            ok = resolver.resolve(probe, ROUTE_USB).status == READY
            try:
                signals.usb_available.emit(watch_id, ok)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_usb_available(self, watch_id: int, ok: bool):
        if watch_id != self._watch_id or not self._streaming:
            return
        self._switch_usb_row.setVisible(ok)

    def _switch_to_usb(self):
        self._switch_usb_row.setVisible(False)
        self._host.reconnect_stream()  # automatic routing picks USB now that it answers

    # ── Phones ────────────────────────────────────────────────────────────

    def open_add_phone(self):
        if self._add_dlg is not None and self._add_dlg.isVisible():
            self._add_dlg.raise_()
            self._add_dlg.activateWindow()
            return
        self._add_dlg = AddPhoneDialog(self._host, self._computer_id, self._computer_name,
                                       self._on_phone_paired)
        self._add_dlg.setWindowModality(Qt.WindowModality.NonModal)
        self._add_dlg.show()

    def open_phones(self):
        if self._phones_dlg is None or not self._phones_dlg.isVisible():
            self._phones_dlg = PhonesDialog(self, self._host)
            self._phones_dlg.setWindowModality(Qt.WindowModality.NonModal)
        self._phones_dlg.refresh()
        self._phones_dlg.show()
        self._phones_dlg.raise_()

    def _on_phone_paired(self, result):
        existing = self.phone(result.phone_id)
        active = result.source_ip if result.source_ip in result.ips else None
        if existing:
            existing.token = result.token  # re-pairing replaced this computer's token on the phone
            existing.ips = list(result.ips)
            existing.active_ip = active
            if self._streaming and existing.id == self._selected_id:
                self._host.reconnect_stream()  # the running stream still holds the old token
        else:
            self._phones.append(Phone(result.phone_id, result.name, result.token, list(result.ips), active))
        self._select(result.phone_id, force=True)
        self._host.save_now()
        if self._phones_dlg is not None and self._phones_dlg.isVisible():
            self._phones_dlg.refresh()

    def rename_phone(self, pid: str, name: str):
        phone = self.phone(pid)
        if phone:
            phone.name = name
            self._refresh_combo()
            self._host.save_now()
            self._render()

    def forget_phone(self, pid: str):
        phone = self.phone(pid)
        if phone is None:
            return
        self._spawn_revoke(Phone(**phone.to_dict()))
        self._phones = [p for p in self._phones if p.id != pid]
        if pid == self._selected_id:
            self._active_key = None  # nothing to save back: its settings are being deleted
            self._select(self._phones[0].id if self._phones else None)
        else:
            self._refresh_combo()
        self._host.forget_device_settings(pid)
        self._host.save_now()

    def _spawn_revoke(self, phone: Phone):
        """Unpair on the phone too when it's reachable, so it stops accepting a forgotten computer."""
        resolver = self._resolver

        def revoke():
            res = resolver.resolve(phone, ROUTE_AUTO)
            if res.route is None:
                return
            with self.session_channel(SessionTarget(phone.token, res.route)) as client:
                if client is not None:
                    client.unpair()
        threading.Thread(target=revoke, daemon=True).start()

    def _select(self, pid: Optional[str], force: bool = False):
        if pid == self._selected_id and not force:
            return
        if self._streaming and pid != self._selected_id:
            self._host.stop_stream()
        self._selected_id = pid
        self._resolution = None
        self._refresh_combo()
        self._activate_profile(pid)
        self._render()
        self._check_status()

    def _on_combo_changed(self, idx: int):
        if self._switching:
            return
        pid = self._phone_combo.itemData(idx)
        if pid:
            self._select(pid)

    def _activate_profile(self, new_key: Optional[str]):
        """Swap per-phone settings via the host, only when the phone actually changed."""
        if new_key == self._active_key:
            return
        prev, self._active_key = self._active_key, new_key
        self._host.switch_device(prev, new_key)

    def sync_active_profile(self):
        """Record the restored selection so the first real change is the one that switches profiles."""
        self._active_key = self._selected_id

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {
            "computer_id": self._computer_id,
            "computer_name": self._computer_name,
            "route": self._route_pref,
            "phones": [p.to_dict() for p in self._phones],
            "selected_phone": self._selected_id,
        }

    def set_config(self, cfg: dict):
        cid = cfg.get("computer_id")
        if isinstance(cid, str) and cid:
            self._computer_id = cid
        name = cfg.get("computer_name")
        if isinstance(name, str) and name.strip():
            self._computer_name = name.strip()
        route = cfg.get("route")
        self._route_pref = route if route in (ROUTE_AUTO, ROUTE_USB, ROUTE_WIFI) else ROUTE_AUTO
        self._phones = []
        raw_phones = cfg.get("phones")
        for raw in raw_phones if isinstance(raw_phones, list) else []:
            try:
                self._phones.append(Phone.from_dict(raw))
            except ValueError:
                logger.warning("Discarding malformed phone entry in config: %r", raw)
        sel = cfg.get("selected_phone")
        self._selected_id = sel if self.phone(sel) else (self._phones[0].id if self._phones else None)
        self._resolution = None
        self._refresh_combo()
        self._render()
        self._check_status()

    def shutdown(self):
        self._discovery.stop()
