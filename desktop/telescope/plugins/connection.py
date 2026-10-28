"""Paired phones, how to reach them, and pairing new ones.

The user-facing model: you add phones (one dialog: scan a code, or just plug in over USB), pick which
one to use, and press Start. How it's reached is decided per connection by phones.RouteResolver:
USB whenever this phone answers over a cable, Wi-Fi otherwise, with the reason shown next to the
route and a one-click override. Tokens, ports and adb forwards never reach the UI.
"""

import base64
import contextlib
import html
import logging
import secrets
import socket
import threading
import time
from dataclasses import dataclass
from typing import Optional

from PyQt6.QtCore import QObject, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QFont
from PyQt6.QtWidgets import (
    QButtonGroup, QFrame, QHBoxLayout, QInputDialog, QLabel, QLineEdit,
    QMessageBox, QPushButton, QScrollArea, QVBoxLayout, QWidget,
)

from telescope import h264_reader, theme
from telescope.discovery import LanDiscovery
from telescope.pairing import PairingServer
from telescope.pinned_https import PhoneAuth
from telescope.phones import (
    DESKTOP_OUTDATED, LOCAL_ONLY, NOT_PAIRED, PHONE_OUTDATED, READY, ROUTE_AUTO, ROUTE_USB, ROUTE_WIFI, STREAM_PORT, UNREACHABLE,
    USB_NEEDS_ATTENTION, USB_NO_ADB, USB_NO_CABLE, Phone, Resolution, Route, RouteResolver, UsbTunnels, usb_note_text,
)
from telescope.platform import (
    IS_LINUX, IS_WINDOWS, adb_available, adb_broadcast_pair, adb_device_states, adb_forward_auto, adb_install,
    adb_reverse, adb_unforward, adb_unreverse, bundled_apk_path, stop_adb_server,
)
from telescope.platform.linux import (
    CANCELLED, V4L2_PHONE_DEV, v4l2_devices_ready, v4l2_module_loaded, v4l2_setup,
)
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.shortcuts import choice_action
from telescope.session_client import (
    PING_PORT, START_FAST_POLL_FOR, START_FAST_POLL_INTERVAL, START_POLL_INTERVAL, START_TIMEOUT, MAX_NAME_CHARS,
    PhoneSessionClient, clean_name,
)
from telescope.widgets.controls import RoundIcon, Tag, hairline, setting_row
from telescope.widgets.line_icons import line_icon
from telescope.widgets.common import (
    set_button_icon,
    SegmentButton, segmented_row,
    Switch,
    ElidingLabel, NoScrollComboBox, PlacedDialog, action_button, add_card_header, card_action,
    card_layout, control_row_widget, create_card, create_vector_icon,
    dialog_buttons, dialog_header, dialog_layout, icon_button, run_off_ui_thread, set_status_kind, set_ui_role,
    ui_px, wrapped_note,
)
from telescope.widgets.banner import BannerAction, Issue, copy_action
from telescope.widgets.adb import GET_ADB_LINK, PICK_ADB_LINK, AdbDownload
from telescope.widgets.qr import QRCodeWidget
from telescope.i18n import N_, tr

logger = logging.getLogger(__name__)

# Translators: Light is the name of a stream format, as the Stream output card shows it
_UDP_SUB = N_("Sends Light over UDP instead of TCP while the phone is on Wi-Fi. It can stutter more than TCP, so try it "
             "and see.")
_STATUS_POLL_MS = 3_000     # idle re-check of the selected phone, only while the window is on screen
_CHECK_GIVE_UP_S = 60       # an idle check that never answers stops blocking new ones
_HELD_POLL_S = 60           # tray, after starting once the phone was ready: only its going away matters
_USB_WATCH_MS = 5_000       # while streaming over Wi-Fi: has the phone been plugged in?
_USB_SIT_OUT_S = 10         # USB that just failed a stream isn't tried again for this long, so it can't flap
_PAIR_USB_POLL_MS = 2_000   # Add phone dialog: look for a phone on USB to pair with

_MAX_REMEMBERED_IPS = 8     # a phone's addresses kept for Wi-Fi probes, newest last

# Tolerated unreachable pings while the camera starts; startup can briefly starve the phone's HTTP server.
_UNREACHABLE_STREAK_LIMIT = 3


def default_computer_name() -> str:
    return clean_name(socket.gethostname().split(".")[0]) or tr("Computer")


def _ask_name(parent, title: str, label: str, text: str):
    """QInputDialog.getText with the name length cap on its line edit; returns (text, accepted)."""
    dlg = QInputDialog(parent)
    dlg.setWindowTitle(title)
    dlg.setLabelText(label)
    dlg.setTextValue(text)
    edit = dlg.findChild(QLineEdit)
    if edit is not None:
        edit.setMaxLength(MAX_NAME_CHARS)
    accepted = dlg.exec() == QInputDialog.DialogCode.Accepted
    return dlg.textValue(), accepted


@dataclass(frozen=True)
class SessionTarget:
    """What worker threads need to talk to the phone, snapshotted on the GUI thread."""
    auth: Optional[PhoneAuth]
    route: Optional[Route]


def busy_other_reason(computer: Optional[str]) -> str:
    """Why Start didn't take the phone: another paired computer's stream is running on it."""
    text = (tr("The phone is streaming to {computer}.", computer=computer) if computer
            else tr("The phone is streaming to another computer."))
    return text + "\n\n" + tr("Stop it there or on the phone, then try again.")


# ── Status text ───────────────────────────────────────────────────────────────

def status_line(res: Optional[Resolution]) -> tuple:
    """(status kind, short text) for the Connection card's Phone row."""
    if res is None:
        return "status_dim", tr("Checking…")
    return {
        READY: ("status_ok", tr("Ready")),
        UNREACHABLE: ("status_warn", "○ " + tr("Can't reach the phone")),
        NOT_PAIRED: ("status_err", "○ " + tr("Needs pairing again")),
        LOCAL_ONLY: ("status_warn", "○ " + tr("Phone accepts USB only")),
        USB_NEEDS_ATTENTION: ("status_warn", "○ " + tr("USB not available")),
        PHONE_OUTDATED: ("status_warn", "○ " + tr("Phone app needs an update")),
        DESKTOP_OUTDATED: ("status_warn", "○ " + tr("This app needs an update")),
    }.get(res.status, ("status_dim", ""))


def problem_text(res: Resolution, phone_name: str, preference: str) -> str:
    """What's wrong and what to do about it, for the card note and for Start failures."""
    if res.status == UNREACHABLE:
        return tr("Open Telescope on {phone} and keep it on screen. It needs to be on the same network as this "
                  "computer, or plugged in with a USB cable.", phone=phone_name)
    if res.status == NOT_PAIRED and res.before_tls:  # LEGACY MIGRATION PATH
        return tr("This version of Telescope encrypts the connection to {phone}, which needs a new pairing. Click "
                  "Add phone and scan the code again.", phone=phone_name)
    if res.status == NOT_PAIRED:
        return tr("{phone} doesn't recognise this computer anymore (it was removed on the phone, or the app was "
                  "reinstalled). Click Add phone to pair it again.", phone=phone_name)
    if res.status == LOCAL_ONLY:
        text = tr("Local only is on in the phone app, so {phone} only accepts USB. Plug it in with a cable, or turn "
                  "Local only off on the phone.", phone=phone_name)
        if IS_WINDOWS and not adb_available():
            text += " " + tr("USB needs adb, which Add phone can get.")
        return text
    if res.status == USB_NEEDS_ATTENTION:
        why = usb_note_text(res.usb_note) or tr("the phone isn't answering over USB")
        return tr("The connection is set to USB only: {why}. Switch to Automatic to use Wi-Fi instead.", why=why)
    if res.status == PHONE_OUTDATED:
        return tr("The Telescope app on {phone} is older than this one. Update it on the phone.", phone=phone_name)
    if res.status == DESKTOP_OUTDATED:
        return tr("The Telescope app on {phone} is newer than this one. Update Telescope on this computer.",
                  phone=phone_name)
    return ""


def route_text(route: Optional[Route]) -> str:
    if route is None:
        return "—"
    return tr("USB cable") if route.kind == "usb" else f"Wi-Fi · {route.host}"


_ROUTE_CHOICES = ((ROUTE_AUTO, N_("Automatic")), (ROUTE_USB, N_("USB only")), (ROUTE_WIFI, N_("Wi-Fi only")))


# ── Add phone ─────────────────────────────────────────────────────────────────

class _PairSignals(QObject):
    paired = pyqtSignal(object)        # PairingResult
    usb_state = pyqtSignal(object)     # list of (serial, state) from adb


class AddPhoneDialog(PlacedDialog):
    """One way in: scan the code over Wi-Fi, or plug the phone in and it pairs over USB by itself."""

    def __init__(self, parent, computer_id: str, computer_name: str, on_paired):
        super().__init__(parent)
        self.setWindowTitle(tr("Add phone"))
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumWidth(ui_px(460))
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
        self._subtitle = dialog_header(lay, tr("Add a phone"),
                                       tr("Open Telescope on the phone first."))

        self._qr_container = QVBoxLayout()
        self._qr_container.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        lay.addLayout(self._qr_container)

        self._wifi_lbl = wrapped_note(tr("Starting…"))
        self._wifi_row = control_row_widget("Wi-Fi", self._wifi_lbl, stretch=True)
        lay.addWidget(self._wifi_row)

        self._usb_lbl = wrapped_note("")
        set_status_kind(self._usb_lbl, "status_dim")
        self._usb_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.LinksAccessibleByMouse)
        self._usb_lbl.linkActivated.connect(self._get_adb)
        self._usb_row = control_row_widget("USB", self._usb_lbl, stretch=True)
        lay.addWidget(self._usb_row)
        self._adb_download = AdbDownload(self)
        self._adb_download.progress.connect(lambda msg: self._set_usb(html.escape(msg), "status_dim"))
        self._adb_download.finished.connect(self._on_adb_downloaded)

        self._result_lbl = QLabel("")
        self._result_lbl.setObjectName("dialog_title")
        self._result_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._result_lbl.setVisible(False)
        lay.addWidget(self._result_lbl)

        self._close_btn = QPushButton(tr("Cancel"))
        self._close_btn.clicked.connect(self.reject)
        dialog_buttons(lay, self._close_btn)

    def setVisible(self, visible):
        # The code goes in before the first show, so the window opens at the size that fits it.
        if visible:
            self._start()
            self.adjustSize()
        super().setVisible(visible)

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
            # Translators: "Scan pairing code" is a button in the phone app: use the phone app's translation of it
            self._wifi_lbl.setText(tr("On the phone, tap Scan pairing code and point it at this code."))
        else:
            self._wifi_lbl.setText(tr("This computer isn't on a network right now, so pair over USB."))
        self._start_usb()

    def _start_usb(self):
        if self._adb_download.running:  # reopened mid-download: its progress shows on the next step
            self._set_usb(tr("Getting adb from Google…"), "status_dim")
        elif adb_available():
            self._set_usb(tr("Plug the phone in with a USB cable and it pairs by itself."), "status_dim")
            self._usb_timer.start(_PAIR_USB_POLL_MS)
            self._poll_usb()
        elif IS_WINDOWS:
            # Translators: keep the <a href="{link}"> link
            self._set_usb(tr('Needs adb, which Telescope can <a href="{link}">get from Google</a>. Wi-Fi works '
                             'without it.', link=GET_ADB_LINK), "status_dim")
        else:
            self._set_usb(tr("Needs adb, which isn't installed. Wi-Fi works without it."), "status_dim")

    def _get_adb(self, link: str):
        if link == GET_ADB_LINK and not self._done:
            self._adb_download.ask_and_start(self)
        elif link == PICK_ADB_LINK and not self._done:
            self._adb_download.pick_and_start(self, lambda: self._set_usb(tr("Copying adb…"), "status_dim"))

    def _on_adb_downloaded(self, ok: bool, detail: str):
        if self._done or self._server is None:  # closed meanwhile: opening it again starts USB if adb is there
            return
        if ok:
            self._start_usb()
        else:
            # Translators: keep both <a href="..."> links
            self._set_usb(html.escape(detail) + " " + tr('<a href="{get}">Try again</a> or <a href="{pick}">pick one '
                                                         'you already have</a>.', get=GET_ADB_LINK, pick=PICK_ADB_LINK),
                          "status_err")

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
                self._set_usb(tr("Phone plugged in. Allow USB debugging in the prompt on the phone."), "status_warn")
            else:
                self._set_usb(tr("Plug the phone in with a USB cable and it pairs by itself."), "status_dim")
            return
        self._set_usb(tr("Phone plugged in. Keep Telescope open on it; pairing…"), "status_dim")
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
        self._subtitle.setText(tr("You can start streaming now."))
        self._result_lbl.setText(tr("Paired with {name}", name=result.name))
        self._result_lbl.setVisible(True)
        self._close_btn.setText(tr("Done"))
        set_ui_role(self._close_btn, "primary")
        self._close_btn.clicked.disconnect()
        self._close_btn.clicked.connect(self.accept)
        self.adjustSize()
        self._on_paired(result)


# ── Phones list ───────────────────────────────────────────────────────────────

class _PhoneRows(QScrollArea):
    """The phones' strips: as tall as they are up to about six of them, then a scroll bar instead of a dialog that
    grows off the screen."""
    MAX_HEIGHT = 320
    GUTTER = 14

    def __init__(self):
        super().__init__()
        self.setWidgetResizable(True)
        self.setFrameShape(QFrame.Shape.NoFrame)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.setStyleSheet("QScrollArea, QScrollArea > QWidget, QScrollArea > QWidget > QWidget "
                           "{ background: transparent; }")  # the card shows through, not the window's colour
        rows = QWidget()
        self.rows = QVBoxLayout(rows)
        self.rows.setContentsMargins(0, 0, 0, 0)
        self.rows.setSpacing(8)
        self.rows.addStretch(1)
        self.setWidget(rows)
        # Room for the scroll bar once there is one, so it doesn't cover the rows' buttons.
        self.verticalScrollBar().rangeChanged.connect(self._on_range)

    def _on_range(self, _lo: int, hi: int):
        self.rows.setContentsMargins(0, 0, ui_px(self.GUTTER) if hi > 0 else 0, 0)

    def add(self, row: QWidget):
        self.rows.insertWidget(self.rows.count() - 1, row)
        row.show()  # a layout only shows it on the next pass, and fit() would measure the list without it

    def fit(self):
        """As tall as the rows at the width they get, less the scroll bar's room so a list that just fits doesn't
        grow one, get narrower and then not fit."""
        self.widget().ensurePolished()
        lay = self.widget().layout()
        width = self.viewport().width() - (0 if self.verticalScrollBar().maximum() > 0 else ui_px(self.GUTTER))
        wanted = lay.totalHeightForWidth(width) if lay.hasHeightForWidth() and width > 0 else lay.sizeHint().height()
        self.setFixedHeight(min(wanted, ui_px(self.MAX_HEIGHT)))


class PhonesDialog(PlacedDialog):
    def __init__(self, plugin: "ConnectionPlugin", parent=None):
        super().__init__(parent)
        self._plugin = plugin
        self.setWindowTitle(tr("Your phones"))
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumWidth(ui_px(460))
        lay = dialog_layout(self)
        dialog_header(lay, tr("Your phones"),
                      tr("Each phone keeps its own camera settings, and so does a browser once you change one. "
                         "Removing a phone also unpairs it on the phone."))

        phones_card = create_card()
        phones_lay = card_layout(phones_card)
        self._add_btn = card_action(tr("Add phone"), "plus", tr("Pair another phone"))
        self._add_btn.clicked.connect(plugin.open_add_phone)
        add_card_header(phones_lay, tr("Phones"), "phone", action=self._add_btn, hue=ConnectionPlugin.category)
        self._scroll = _PhoneRows()
        phones_lay.addWidget(self._scroll)
        self._rows: dict = {}  # id -> its strip
        self._empty = wrapped_note(tr("No phones yet. Add one to pair it with this computer."))
        phones_lay.addWidget(self._empty)
        lay.addWidget(phones_card)

        computer_card = create_card()
        computer_lay = card_layout(computer_card)
        add_card_header(computer_lay, tr("This computer"), "monitor", hue=ConnectionPlugin.category)
        rename_computer = QPushButton(tr("Change name"))
        rename_computer.setToolTip(tr("The name your phones show for this computer"))
        rename_computer.clicked.connect(self._rename_computer)
        strip, _ = setting_row("", tr("What your phones call it"), rename_computer)
        self._computer_lbl = strip.findChild(QLabel, "setting_name")
        rename_computer.setAccessibleName(tr("Change this computer's name"))
        computer_lay.addWidget(strip)
        lay.addWidget(computer_card)
        lay.addStretch(1)

        close_btn = QPushButton(tr("Close"))
        close_btn.clicked.connect(self.accept)
        dialog_buttons(lay, close_btn)
        self.refresh()

    def refresh(self):
        self._computer_lbl.setText(self._plugin.computer_name)
        for row in self._rows.values():
            row.hide()  # deleteLater waits for the event loop, and until then it'd still take up room
            row.deleteLater()
        self._rows = {}
        for phone in self._plugin.phones:
            ip = phone.active_ip or (phone.ips[0] if phone.ips else "")
            self._add_row(phone.id, phone.name, tr("Phone · {ip}", ip=ip) if ip else tr("Phone"), renamable=True)
        for sid, name, detail in self._plugin.remembered:  # browsers with settings of their own
            self._add_row(sid, name, detail, renamable=False)
        self._empty.setVisible(not self._rows)
        self._scroll.setVisible(bool(self._rows))
        self._fit()

    def _fit(self):
        self._scroll.fit()
        if self.isVisible():
            self.layout().invalidate()  # Qt doesn't resize a window whose layout wraps text by itself
            self.adjustSize()

    def showEvent(self, event):
        super().showEvent(event)
        QTimer.singleShot(0, self._fit)  # again now the rows know their real width (a pixel or two at most)

    def _add_row(self, sid: str, name: str, sub: str, renamable: bool):
        buttons = QHBoxLayout()
        buttons.setContentsMargins(0, 0, 0, 0)
        buttons.setSpacing(6)
        if renamable:
            rename = icon_button("pencil", tr("Rename {name}", name=name), tr("Rename"))
            rename.setObjectName("rename")
            rename.clicked.connect(lambda _c=False, i=sid: self._rename(i))
            buttons.addWidget(rename)
        remove = icon_button("trash", tr("Remove {name}", name=name), tr("Remove"))
        remove.setObjectName("remove")
        remove.clicked.connect(lambda _c=False, i=sid: self._remove(i))
        buttons.addWidget(remove)
        strip, _ = setting_row(name, sub, buttons)
        self._scroll.add(strip)
        self._rows[sid] = strip

    def _browser(self, sid: Optional[str]) -> Optional[tuple]:
        return next((e for e in self._plugin.remembered if e[0] == sid), None)

    def _rename(self, pid: str):
        phone = self._plugin.phone(pid)
        if phone is None:
            return
        name, ok = _ask_name(self, tr("Rename phone"), tr("Name"), phone.name)
        if ok and name.strip():
            self._plugin.rename_phone(pid, name.strip())
            self.refresh()

    def _rename_computer(self):
        name, ok = _ask_name(self, tr("Rename this computer"),
                             tr("Name your phones show (applies to phones you pair from now on)"),
                             self._plugin.computer_name)
        if ok and name.strip():
            self._plugin.set_computer_name(name.strip())
            self.refresh()

    def _remove(self, pid: str):
        if (browser := self._browser(pid)) is not None:
            r = QMessageBox.question(
                self, tr("Remove browser"),
                tr('Remove "{name}"? This deletes the settings it had on this computer. It can still connect with '
                   'the code, and starts from the defaults.', name=browser[1]),
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No, QMessageBox.StandardButton.No)
            if r == QMessageBox.StandardButton.Yes:
                self._plugin.forget_source(pid)
                self.refresh()
            return
        phone = self._plugin.phone(pid)
        if phone is None:
            return
        r = QMessageBox.question(
            self, tr("Remove phone"),
            tr('Remove "{name}"? This deletes its camera settings on this computer, and unpairs it on the phone too '
               'if Telescope can reach it.', name=phone.name),
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if r == QMessageBox.StandardButton.Yes:
            self._plugin.forget_phone(pid)
            self.refresh()


# ── Plugin ────────────────────────────────────────────────────────────────────

class _Signals(QObject):
    resolved = pyqtSignal(int, object, object)   # check id, phone id, Resolution
    usb_available = pyqtSignal(int, object)      # watch id, the USB route our phone answers on (None: it doesn't)
    phone_updated = pyqtSignal(bool, str)        # ok, what went wrong
    route_probed = pyqtSignal(int, object, str, object)  # probe id, phone id, Connect via choice, Resolution


class ConnectionPlugin(TelescopePlugin):
    name = "connection"
    title = N_("Phones and connection")
    about = N_("Pairing phones, picking one and connecting to it over Wi-Fi or USB.")
    category = "streaming"
    icon = "connection"
    pinned = True
    required = True

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._phones: list = []
        self._selected_id: Optional[str] = None
        self._active_key: Optional[str] = None
        self._route_pref = ROUTE_AUTO
        self._udp = False  # experimental H.264 over UDP on Wi-Fi
        self._transport_udp = False
        self._udp_dynamic: set = set()  # phones that run Dynamic over UDP, from their last ping
        self._udp_cb = None
        self._pending_route: Optional[str] = None  # a Connect via choice checked before the stream moves to it
        self._route_probe_id = 0
        self._computer_id = secrets.token_hex(16)
        self._computer_name = default_computer_name()
        self._resolution: Optional[Resolution] = None
        self._check_id = 0
        self._no_phone_issue: Optional[Issue] = None  # the "No phone yet" banner, while it may still be up
        self._running_check: Optional[int] = None  # the idle check still resolving; the poll waits for it
        self._check_started = 0.0
        self._watch_id = 0
        self._streaming = False
        self._connected = False  # first frame arrived (EventBus.stream_connected)
        self._lost = False       # frames stopped; the host is reconnecting (EventBus.stream_lost)
        # The route each streaming phone's video comes over, and the USB forward it holds (None over Wi-Fi), by id
        self._holds: dict = {}
        self._paths: dict = {}  # pid: the video path its stream last opened, which says its format
        self._prepared: dict = {}  # pid: (route, USB forward serial or None, path) a hand-over is trying
        self._usb_sat_out = -1e9  # monotonic time USB last failed a stream, which then sits out USB_SIT_OUT_S
        self._switching = False
        self._combo_stale = False  # sources changed while the picker's list was open
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
        self._signals.phone_updated.connect(self._on_phone_updated)
        self._signals.route_probed.connect(self._on_route_probed)
        self._updating_phone = False
        self._update_note: Optional[tuple] = None  # (kind, text) from the last phone update
        bus.stream_connected.connect(self._on_stream_connected)
        bus.stream_lost.connect(self._on_stream_lost)
        bus.stream_transport_changed.connect(self._refresh_transport)
        bus.add_phone_requested.connect(self.open_add_phone)
        self._sources: list = []  # [(id, name)] of the StreamSources other plugins offer (Browser camera)
        bus.stream_sources_changed.connect(self._on_sources)
        self.remembered: list = []  # [(id, name, detail)] of browsers with settings of their own
        bus.remembered_sources.connect(self._on_remembered)
        # Each idle check wakes the phone's Wi-Fi and CPU, so a phone left waiting all day isn't polled while nobody
        # looks. An app reading the camera, or starting when the phone is ready, keeps the checks going, so those
        # still see the phone arrive.
        self._shown = True  # the window says otherwise when it starts hidden
        self._watched = False
        self._wanted = False
        self._wanted_held = False  # it already started for this arrival: slow checks will do
        bus.window_shown.connect(lambda shown: self._set_poll_reason("_shown", shown))
        bus.camera_watched.connect(lambda watched: self._set_poll_reason("_watched", watched))
        bus.phone_wanted.connect(self._on_phone_wanted)

    def _on_remembered(self, entries: list):
        self.remembered = list(entries)
        if self._phones_dlg is not None and self._phones_dlg.isVisible():
            self._phones_dlg.refresh()

    def forget_source(self, sid: str):
        self._bus.forget_source_requested.emit(sid)

    # ── Model ─────────────────────────────────────────────────────────────

    @property
    def phones(self) -> list:
        return list(self._phones)

    def phone(self, pid: Optional[str]) -> Optional[Phone]:
        return next((p for p in self._phones if p.id == pid), None)

    def _selected_phone(self) -> Optional[Phone]:
        return self.phone(self._selected_id)

    def _source_name(self, sid: Optional[str]) -> Optional[str]:
        return next((name for i, name in self._sources if i == sid), None)

    def _on_sources(self, sources: list):
        self._sources = list(sources)
        self._refresh_combo()
        self._render()
        self._announce_source()

    def on_plugins_ready(self):
        self._bus.phones_changed.emit(len(self._phones))
        self._announce_source()

    def _announce_source(self):
        self._bus.source_selected.emit(self._selected_id if self._source_name(self._selected_id) else "")

    @property
    def _stream_route(self) -> Optional[Route]:
        """The route the selected phone streams over."""
        return self._holds.get(self._selected_id, (None, None))[0]

    @_stream_route.setter
    def _stream_route(self, route: Optional[Route]):
        self._holds[self._selected_id] = (route, self._holds.get(self._selected_id, (None, None))[1])

    @property
    def selected_device(self) -> Optional[str]:
        """Key the host stores per-phone settings under: the selected phone's id."""
        return self._selected_id

    @property
    def computer_name(self) -> str:
        return self._computer_name

    def set_computer_name(self, name: str):
        self._computer_name = clean_name(name) or self._computer_name
        self._host.save_now()

    @property
    def resolution(self) -> Optional[Resolution]:
        return self._resolution

    # ── UI ────────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._add_btn = card_action(tr("Add phone"), "plus", tr("Pair a phone over Wi-Fi or USB"))
        self._add_btn.clicked.connect(self.open_add_phone)
        header = add_card_header(lay, tr("Connection"), self.icon, action=self._add_btn, hue=self.category)
        self._guide_btn = card_action("", "help", tr("Open or close the setup guide: the virtual camera, the phone app "
                                                     "and pairing"))
        self._guide_btn.setAccessibleName(tr("Setup guide"))
        self._guide_btn.clicked.connect(self._bus.setup_guide_requested.emit)
        header.insertWidget(header.count() - 1, self._guide_btn)

        # The link as one block: how the phone is doing and over what, then the two choices about the route.
        link = QFrame()
        link.setObjectName("subsection")
        link_lay = QVBoxLayout(link)
        link_lay.setContentsMargins(12, 8, 12, 6)
        link_lay.setSpacing(4)
        top = QHBoxLayout()
        top.setSpacing(12)
        self._route_icon = RoundIcon(34)
        top.addWidget(self._route_icon)
        texts = QVBoxLayout()
        texts.setSpacing(1)
        self._status_lbl = ElidingLabel("")
        status_font = self._status_lbl.font()
        status_font.setWeight(QFont.Weight.DemiBold)
        status_font.setPointSizeF(status_font.pointSizeF() * 1.08)
        self._status_lbl.setFont(status_font)
        texts.addWidget(self._status_lbl)
        self._using_lbl = ElidingLabel("—")
        self._using_lbl.setObjectName("dim")
        self._using_row = self._using_lbl
        texts.addWidget(self._using_lbl)
        top.addLayout(texts, 1)
        link_lay.addLayout(top)
        # What's wrong and what to do, lined up with the status it explains
        self._note_box = QWidget()
        note_lay = QVBoxLayout(self._note_box)
        note_lay.setContentsMargins(self._route_icon.width() + top.spacing(), 0, 0, 2)
        note_lay.setSpacing(8)
        self._note_lbl = wrapped_note("")
        note_lay.addWidget(self._note_lbl)
        self._switch_usb_btn = action_button(tr("Switch to USB"), "primary",
                                             tr("The phone was plugged in; reconnect over the cable"))
        self._switch_usb_btn.clicked.connect(self._switch_to_usb)
        self._switch_usb_btn.setVisible(False)
        note_lay.addWidget(self._switch_usb_btn, 0, Qt.AlignmentFlag.AlignLeft)
        self._update_btn = action_button(tr("Update over USB"), "primary")
        self._update_btn.clicked.connect(self._update_action)
        self._update_btn.setVisible(False)
        note_lay.addWidget(self._update_btn, 0, Qt.AlignmentFlag.AlignLeft)
        self._note_row, self._switch_usb_row, self._update_row = self._note_lbl, self._switch_usb_btn, self._update_btn
        link_lay.addWidget(self._note_box)
        link_lay.addSpacing(4)
        link_lay.addWidget(hairline())
        self._show_route_icon(None)

        self._route_combo = NoScrollComboBox()
        for key, label in _ROUTE_CHOICES:
            self._route_combo.addItem(tr(label), key)
        self._route_combo.setToolTip(tr("Automatic: USB when the phone's plugged in, Wi-Fi otherwise."))
        self._route_combo.currentIndexChanged.connect(
            lambda i: self.set_route_preference(self._route_combo.itemData(i)))
        self._route_combo.setVisible(False)  # the segments below drive it; it stays for shortcuts
        self._route_segs = {}
        self._route_grp = QButtonGroup(link)
        for key, text, icon in ((ROUTE_AUTO, tr("Auto"), ""), (ROUTE_USB, "USB", "usb"), (ROUTE_WIFI, "Wi-Fi", "wifi")):
            btn = SegmentButton(text)
            if icon:
                set_button_icon(btn, line_icon(icon, theme.TEXT_DIM), 13)
            btn.setToolTip(self._route_combo.toolTip())
            btn.clicked.connect(lambda _c=False, k=key: self._route_combo.setCurrentIndex(self._route_combo.findData(k)))
            self._route_grp.addButton(btn)
            self._route_segs[key] = btn
        self._route_row = control_row_widget(tr("Connect via"), segmented_row(*self._route_segs.values()), stretch=True)
        self._route_row.layout().addWidget(self._route_combo)
        link_lay.addWidget(self._route_row)

        lay.addWidget(link)

        self._build_header()

        self._status_timer = QTimer(card)
        self._status_timer.timeout.connect(self._poll_status)
        self._status_timer.start(_STATUS_POLL_MS)
        self._usb_watch_timer = QTimer(card)
        self._usb_watch_timer.timeout.connect(self._watch_usb)
        self._render()
        return card

    def create_settings_sections(self) -> list:
        self._settings_card = card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Connection"), self.icon, hue=self.category)
        self._udp_cb = Switch()
        self._udp_cb.setChecked(self._udp)
        self._udp_cb.setAccessibleName(tr("UDP on Wi-Fi"))
        self._udp_cb.toggled.connect(self._on_udp)
        control = QHBoxLayout()
        control.setSpacing(8)
        control.addWidget(Tag(tr("Experimental"), "warn"))
        control.addWidget(self._udp_cb)
        strip, _ = setting_row(tr("UDP on Wi-Fi"), tr(_UDP_SUB), control,
                               keywords=tr("udp tcp transport network lag freezes stutter wifi", ctx="search"))
        lay.addWidget(strip)
        return [SettingsSection("streaming", card, on_show=self._show_udp)]

    def _show_udp(self):
        self._udp_cb.blockSignals(True)
        self._udp_cb.setChecked(self._udp)
        self._udp_cb.blockSignals(False)

    def create_actions(self) -> list:
        return [choice_action("phone.pick", tr("Phone"), "Telescope", combo=self._phone_combo)]

    def create_header_widget(self) -> QWidget:
        return self._header_w

    def _build_header(self):
        self._header_w = QWidget()
        self._header_w.setObjectName("card_body")
        lay = QHBoxLayout(self._header_w)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        self._phone_combo = NoScrollComboBox()
        self._phone_combo.setMinimumWidth(ui_px(180))
        self._phone_combo.setPlaceholderText(tr("No phone yet"))
        self._phone_combo.setToolTip(tr("Which phone to stream from"))
        self._phone_combo.setAccessibleName(tr("Phone"))
        self._phone_combo.currentIndexChanged.connect(self._on_combo_changed)
        # On the next turn, so a pick from the closing list is handled first
        self._phone_combo.popup_hidden.connect(lambda: QTimer.singleShot(0, self._refresh_if_stale))
        self._phone_combo.setObjectName("phone_picker")
        phone_icon = QLabel(self._phone_combo)
        phone_icon.setPixmap(line_icon("phone", theme.hue(self.category)).pixmap(14, 14))
        phone_icon.setFixedSize(14, 14)
        phone_icon.move(11, (ui_px(34) - 14) // 2)
        phone_icon.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self._phone_combo.setFixedHeight(ui_px(34))
        lay.addWidget(self._phone_combo)
        self._manage_btn = QPushButton()
        self._manage_btn.setObjectName("icon_btn")
        self._manage_btn.setFixedSize(34, 34)
        self._manage_btn.setIcon(create_vector_icon("devices", theme.TEXT_DIM))
        self._manage_btn.setIconSize(QSize(18, 18))
        self._manage_btn.setToolTip(tr("Your phones: add, rename, remove"))
        self._manage_btn.setAccessibleName(tr("Your phones"))
        self._manage_btn.clicked.connect(self.open_phones)
        lay.addWidget(self._manage_btn)

    def _refresh_combo(self):
        if self._phone_combo.popup_open():
            self._mark_gone_entries()  # rebuilding the list under an open popup breaks it: wait for it to close
            self._combo_stale = True
            self._bus.phones_changed.emit(len(self._phones))
            return
        self._combo_stale = False
        self._switching = True
        self._phone_combo.blockSignals(True)
        self._phone_combo.clear()
        for p in self._phones:
            self._phone_combo.addItem(p.name, p.id)
        if self._sources and self._phones:
            self._phone_combo.insertSeparator(self._phone_combo.count())
        for sid, name in self._sources:
            self._phone_combo.addItem(name, sid)
        self._phone_combo.setCurrentIndex(self._phone_combo.findData(self._selected_id))
        self._phone_combo.blockSignals(False)
        self._switching = False
        self._bus.phones_changed.emit(len(self._phones))

    def _mark_gone_entries(self):
        """Greys out the entries in the open list that went away meanwhile (a webcam unplugged), so they can't be picked."""
        here = {p.id for p in self._phones} | {sid for sid, _name in self._sources}
        model = self._phone_combo.model()
        for row in range(self._phone_combo.count()):
            data = self._phone_combo.itemData(row)
            item = model.item(row) if hasattr(model, "item") else None
            if data and item is not None and data not in here:
                item.setEnabled(False)

    def _refresh_if_stale(self):
        if self._combo_stale and not self._phone_combo.popup_open():
            self._refresh_combo()

    def _show_route_icon(self, kind: Optional[str]):
        """The link block's icon: a cable, Wi-Fi, or a cross while there's no route (the card's own icon is the link)."""
        self._route_icon.set_icon({"usb": "usb", "wifi": "wifi"}.get(kind or "", "x"))

    def _render(self):
        """Card rows from the current phone + latest resolution."""
        phone = self._selected_phone()
        source = self._source_name(self._selected_id)
        if phone is None:
            live = self._streaming and self._connected and not self._lost
            set_status_kind(self._status_lbl, "status_ok" if live else "status_dim")
            if source is None:
                self._status_lbl.setText(tr("No phone yet"))
            elif self._streaming:
                self._status_lbl.setText(tr("Streaming", ctx="status") if live
                                         else tr("Waiting for {source}…", source=source))
            else:
                self._status_lbl.setText(tr("{source}, no phone needed", source=source))
            self._using_row.setVisible(False)
            self._route_row.setVisible(False)
            self._show_route_icon(None)
            self._note_lbl.setText("")
            self._note_row.setVisible(False)
            self._update_row.setVisible(False)
            return
        res = self._resolution
        kind, text = status_line(res)
        if self._streaming:
            if self._lost:
                kind, text = "status_warn", tr("Reconnecting…")
            elif self._connected:
                kind, text = "", tr("Streaming", ctx="status")
                over = getattr(self._stream_route, "kind", None)
                if over:
                    text = tr("Streaming over USB") if over == "usb" else tr("Streaming over Wi-Fi")
            else:
                kind, text = "status_dim", tr("Connecting…")
        set_status_kind(self._status_lbl, kind)
        self._status_lbl.setText(text)
        self._route_row.setVisible(True)
        route = self._stream_route if self._streaming else (res.route if res else None)
        self._using_lbl.setText(route_text(route))
        self._using_row.setVisible(route is not None)  # nothing to say over what until there's a route
        self._show_route_icon(getattr(route, "kind", None))
        self._route_combo.blockSignals(True)
        self._route_combo.setCurrentIndex(self._route_combo.findData(self._pending_route or self._route_pref))
        self._route_combo.blockSignals(False)
        self._route_segs[self._pending_route or self._route_pref].setChecked(True)
        note = ""
        if res is not None and not self._streaming:
            note = problem_text(res, phone.name, self._route_pref)
            if (not note and res.route is not None and res.route.kind == "wifi"
                    and res.usb_note not in (None, USB_NO_CABLE, USB_NO_ADB)):
                # A cable is in but not used: exactly when "why isn't it on USB?" needs an answer.
                reason = usb_note_text(res.usb_note)
                note = tr("Not using USB: {reason}.", reason=reason)
        note_kind = "status_dim"
        if self._update_note is not None:
            note_kind, note = self._update_note
        elif self._updating_phone:
            note = tr("Updating the phone app over USB…")
        set_status_kind(self._note_lbl, note_kind)
        self._note_lbl.setText(note)
        self._note_row.setVisible(bool(note))
        action = None if self._streaming else self._outdated_action(res)
        if action == "usb" and self._update_note is None and not self._updating_phone:
            # the button is the how
            note = tr("The Telescope app on {phone} is older than this one.", phone=phone.name)
            self._note_lbl.setText(note)
        self._update_row.setVisible(action is not None)
        if action is not None:
            self._update_btn.setText(tr("Update this app") if action == "desktop" else tr("Update over USB"))
            self._update_btn.setEnabled(not self._updating_phone)

    @staticmethod
    def _outdated_action(res: Optional[Resolution]) -> Optional[str]:
        """The button an app-version mismatch gets, if there's something to press: "desktop" or "usb"."""
        if res is None:
            return None
        if res.status == DESKTOP_OUTDATED:
            return "desktop"
        if (res.status == PHONE_OUTDATED and res.route is not None and res.route.kind == "usb"
                and bundled_apk_path() is not None and adb_available()):
            return "usb"
        return None

    # ── App versions ──────────────────────────────────────────────────────

    def _update_action(self):
        res = self._resolution
        if res is None:
            return
        if res.status == DESKTOP_OUTDATED:
            self._bus.update_requested.emit()
            return
        if self._updating_phone:
            return  # the Start banner offers this too, while the card's own install is still running
        apk = bundled_apk_path()
        if res.status != PHONE_OUTDATED or res.route is None or res.route.kind != "usb" or apk is None:
            return
        self._updating_phone = True
        self._update_note = None
        self._render()
        signals, serial = self._signals, res.route.serial

        def work():
            try:
                ok, detail = adb_install(serial, apk)
            except Exception as exc:  # always report back, or the card stays on "Updating…" for good
                logger.exception("Phone app install failed")
                ok, detail = False, tr("Couldn't update the phone app: {error}", error=exc)
            try:
                signals.phone_updated.emit(ok, detail)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_phone_updated(self, ok: bool, detail: str):
        self._updating_phone = False
        self._update_note = ("status_ok", tr("Phone app updated. Open Telescope on the phone.")) if ok \
            else ("status_err", detail)
        self._render()
        self._check_status()

    # ── Status checks ─────────────────────────────────────────────────────

    def _set_poll_reason(self, attr: str, on: bool):
        was_polling = self._polling()
        setattr(self, attr, on)
        if self._polling() and not was_polling:
            self._check_status()  # don't make the window wait a whole tick to say where the phone is

    def _on_phone_wanted(self, wanted: bool, held: bool):
        self._wanted_held = held
        self._set_poll_reason("_wanted", wanted)

    def _polling(self) -> bool:
        return self._shown or self._watched or self._wanted

    def _polling_fast(self) -> bool:
        return self._shown or self._watched or (self._wanted and not self._wanted_held)

    def _poll_status(self):
        if self._polling_fast():
            self._check_status()
        elif self._polling() and time.monotonic() - self._check_started >= _HELD_POLL_S:
            self._check_status()

    def _check_status(self):
        if self._streaming:
            return
        phone = self._selected_phone()
        if phone is None:
            self._render()
            return
        if self._running_check == self._check_id and time.monotonic() - self._check_started < _CHECK_GIVE_UP_S:
            return  # a slow check would only be made stale by a new one, and then nothing ever lands
        self._discovery.start()
        self._check_id += 1
        self._running_check = self._check_id
        self._check_started = time.monotonic()
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
        if check_id == self._running_check:
            self._running_check = None
        if check_id != self._check_id or phone_id != self._selected_id:
            return  # stale: a newer check, or the user switched phones meanwhile
        self._apply_resolution(res)
        if not self._streaming:
            # Only from the idle check, never from Start's own resolve, which would start it again.
            self._bus.phone_ready.emit(phone_id, res.status == READY)

    def _apply_resolution(self, res: Resolution):
        if self._update_note is not None and res.status not in (PHONE_OUTDATED, UNREACHABLE):
            self._update_note = None  # the phone moved on from the update; its note is stale
        self._resolution = res
        if res.status == READY and self._selected_id:
            if res.udp_dynamic:
                self._udp_dynamic.add(self._selected_id)
            else:
                self._udp_dynamic.discard(self._selected_id)
        phone = self._selected_phone()
        if phone and res.route is not None and res.route.kind == "wifi" and phone.active_ip != res.route.host:
            phone.active_ip = res.route.host  # remember the address that answered (e.g. after DHCP moved it)
            if res.route.host not in phone.ips:
                # Every status check probes each one, so addresses from networks long gone drop off the front.
                phone.ips = (phone.ips + [res.route.host])[-_MAX_REMEMBERED_IPS:]
            self._host.schedule_save()
        self._render()

    def set_route_preference(self, preference: str):
        if preference == self._route_pref:
            if self._pending_route is not None:  # back to the one in use before the check answered
                self._pending_route = None
                self._route_probe_id += 1
                self._render()
            return
        phone = self._selected_phone()
        if self._streaming and phone is not None and self._host.is_streaming_from(phone.id):
            # Make sure the phone answers the new way before the stream that works now is dropped
            self._pending_route = preference
            self._route_probe_id += 1
            self._render()
            self._spawn_route_probe(self._route_probe_id, Phone(**phone.to_dict()), preference)
            return
        self._use_route_preference(preference)

    def _spawn_route_probe(self, probe_id: int, phone: Phone, preference: str):
        """Background resolve for a new Connect via choice (split out so tests can run it synchronously)."""
        signals, resolver = self._signals, self._resolver

        def work():
            try:
                res = resolver.resolve(phone, preference)
            except Exception:
                logger.exception("Route resolve failed")
                res = Resolution(UNREACHABLE)
            try:
                signals.route_probed.emit(probe_id, phone.id, preference, res)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_route_probed(self, probe_id: int, pid: str, preference: str, res: Resolution):
        if probe_id != self._route_probe_id or preference != self._pending_route:
            return  # another choice since
        self._pending_route = None
        phone = self.phone(pid)
        if (res.status == READY or phone is None or pid != self._selected_id or not self._streaming
                or not self._host.is_streaming_from(pid)):
            self._use_route_preference(preference)
            return
        self._render()
        title = {ROUTE_USB: tr("Can't reach {phone} over USB", phone=phone.name),
                 ROUTE_WIFI: tr("Can't reach {phone} over Wi-Fi", phone=phone.name)}.get(preference) \
            or tr("Can't reach {phone}", phone=phone.name)
        now = "USB" if self._stream_route is not None and self._stream_route.kind == "usb" else "Wi-Fi"
        kept = tr(dict(_ROUTE_CHOICES).get(self._route_pref, self._route_pref))
        hint = {ROUTE_USB: " " + tr("Check the cable and that Telescope is open on the phone."),
                ROUTE_WIFI: " " + tr("Check that it's on the same network as this computer.")}.get(preference, "")
        self._host.show_issue("route", Issue(
            title,
            tr("Still streaming over {now}, so Connect via stays on {kept}.", now=now, kept=kept) + hint,
            [BannerAction(tr("Try again"), lambda: self.set_route_preference(preference))], kind="warn"))

    def _use_route_preference(self, preference: str):
        self._pending_route = None
        self._host.clear_issue("route")
        self._route_pref = preference
        self._resolution = None
        self._host.schedule_save()
        self._render()
        if self._streaming:
            self._host.reconnect_stream()
        else:
            self._running_check = None  # a check for the old route mustn't hold this one back, then land as current
            self._check_status()

    # ── Stream lifecycle (called by the host) ─────────────────────────────

    def ensure_virtual_camera(self, interactive: bool = True) -> bool:
        """Whether the virtual camera is ready to stream to, setting it up (asking first) on Linux."""
        return not IS_LINUX or self._ensure_virtual_camera(interactive)

    def get_stream_info(self, interactive: bool = True, pid: Optional[str] = None) -> tuple:
        """(video URL, auth, ok) for streaming from phone pid (the selected one by default), holding its route."""
        if not self.ensure_virtual_camera(interactive):
            return None, None, False
        pid = self._selected_id if pid is None else pid
        phone = self.phone(pid)
        if phone is None:
            self._no_phone_issue = Issue(tr("No phone yet"), tr("Add a phone to stream from."),
                                         [BannerAction(tr("Add phone"), self.open_add_phone)], kind="warn")
            self._host.show_issue("start", self._no_phone_issue)
            return None, None, False
        res = run_off_ui_thread(self._resolver.resolve, Phone(**phone.to_dict()), self._route_pref)
        if pid == self._selected_id:
            self._check_id += 1  # anything in flight is older than this
            self._apply_resolution(res)
        if res.status != READY:
            self._show_start_problem(res, phone)
            return None, None, False
        url = self._hold_stream_route(res.route, pid)
        if url is None:
            self._host.show_issue("start", Issue(
                tr("Can't connect to the phone"),
                tr("adb couldn't open a connection to the phone. Unplug it, plug it back in and try again."),
                [BannerAction(tr("Try again"), self._host.start_again())]))
            return None, None, False
        return url, phone.auth, True

    def show_problem(self, res: Resolution, pid: Optional[str] = None):
        """Say why the phone won't stream (a dropped stream it won't take back), as a failed Start would."""
        pid = self._selected_id if pid is None else pid
        phone = self.phone(pid)
        if phone is None:
            return
        if pid == self._selected_id:
            self._check_id += 1
            self._apply_resolution(res)
        self._show_start_problem(res, phone)

    def _show_start_problem(self, res: Resolution, phone):
        self._host.show_issue("start", Issue(
            tr("Can't connect to the phone"), problem_text(res, phone.name, self._route_pref),
            self._fix_actions(res, phone.id)))

    def _hold_stream_route(self, route: Route, pid: Optional[str]) -> Optional[str]:
        """Make route pid's stream's, letting go of the previous one's USB forward. The video URL, or None."""
        # The panels' format is the picked phone's; one streaming behind them keeps the one it opened
        path = self._video_path() if pid == self._selected_id else self._paths.get(pid) or self._video_path()
        self.release_stream(pid, moving=True)
        forward = None
        if route.kind == "usb":
            # Always a fresh forward: adb drops them when the cable goes, even if it comes back.
            local = run_off_ui_thread(self._tunnels.acquire, route.serial, STREAM_PORT)
            if local is None:
                return None
            forward = route.serial
            url = f"https://127.0.0.1:{local}{path}"
        else:
            url = f"https://{route.host}:{STREAM_PORT}{self._path_for(route, path, pid)}"
        self._holds[pid] = (route, forward)
        self._paths[pid] = path
        return url

    def release_stream(self, pid: Optional[str], moving: bool = False):
        """pid's stream is over (or moving: going onto another route): let go of its route and USB forward."""
        _route, serial = self._holds.pop(pid, (None, None))
        if serial is not None:
            run_off_ui_thread(self._tunnels.release, serial, STREAM_PORT)
        if not moving:
            self.drop_stream_route(pid)  # a hand-over still under way when it moves can still finish

    def prepare_stream_route(self, route: Route, pid: Optional[str]) -> Optional[str]:
        """The video URL for pid's stream on route, while it still runs on its old one (a hand-over). None if not."""
        if pid not in self._holds or pid in self._prepared:
            return None
        held = self._holds[pid][0]
        if held is not None and held.kind == route.kind and (held.serial, held.host) == (route.serial, route.host):
            return None  # already there
        path = self._paths.get(pid) or self._video_path()
        forward = None
        if route.kind == "usb":
            local = run_off_ui_thread(self._tunnels.acquire, route.serial, STREAM_PORT)
            if local is None:
                return None
            forward = route.serial
            url = f"https://127.0.0.1:{local}{path}"
        else:
            url = f"https://{route.host}:{STREAM_PORT}{self._path_for(route, path, pid)}"
        self._prepared[pid] = (route, forward, path)
        return url

    def commit_stream_route(self, pid: Optional[str]):
        """The hand-over worked: the prepared route is pid's stream's now, and the old one's forward goes."""
        prepared = self._prepared.pop(pid, None)
        if prepared is None:
            return
        route, forward, path = prepared
        _old, old_serial = self._holds.get(pid, (None, None))
        if old_serial is not None:
            run_off_ui_thread(self._tunnels.release, old_serial, STREAM_PORT)
        self._holds[pid] = (route, forward)
        self._paths[pid] = path
        self._render()

    def drop_stream_route(self, pid: Optional[str], failed: bool = False):
        """The hand-over didn't happen: let go of the route it prepared. failed: that route sits out a while."""
        prepared = self._prepared.pop(pid, None)
        if prepared is None:
            return
        route, forward, _path = prepared
        if forward is not None:
            run_off_ui_thread(self._tunnels.release, forward, STREAM_PORT)
        if failed and route.kind == "usb":
            self._usb_sat_out = time.monotonic()
            if pid == self._selected_id and self._streaming:
                self._switch_usb_row.setVisible(True)  # reconnecting over it by hand still might work

    # ── Getting a dropped stream back ─────────────────────────────────────

    def recovery_probe(self, pid: Optional[str] = None):
        """A blocking job for a worker thread: how phone pid (the selected one by default) can be reached right now,
        as a Resolution.

        Snapshots the phone and preference here, on the GUI thread, since the job runs off it.
        """
        phone = self.phone(self._selected_id if pid is None else pid)
        if phone is None:
            return lambda: Resolution(UNREACHABLE)
        probe, preference, resolver = Phone(**phone.to_dict()), self._route_pref, self._resolver
        return lambda: resolver.resolve(probe, preference)

    def adopt_stream_route(self, route: Route, pid: Optional[str] = None) -> Optional[str]:
        """Move pid's running stream onto route (a recovery_probe answer). The new video URL, or None."""
        pid = self._selected_id if pid is None else pid
        if pid not in self._holds:
            return None
        url = self._hold_stream_route(route, pid)
        if url is None and pid not in self._holds:
            self._holds[pid] = (None, None)  # the USB forward failed: keep its place so the next probe tries again
        self._render()
        return url

    def _on_udp(self, on: bool):
        if on == self._udp:
            return
        self._udp = on
        self._host.schedule_save()
        self._refresh_transport()

    def _refresh_transport(self):
        route = self._stream_route
        if not self._streaming or route is None or route.kind != "wifi" or self._video_path() != "/v1/video.h264":
            return
        wants_udp = self._path_for(route, "/v1/video.h264", self._selected_id).endswith(".udp")
        if wants_udp != self._transport_udp:
            self._host.reconnect_stream()

    def _path_for(self, route: Route, path: str, pid: Optional[str] = None) -> str:
        """The H.264 path over UDP where it applies: Wi-Fi and switched on. Dynamic only on a phone that runs it from
        this desktop's reports; an older one measures its TCP queue, so Dynamic stays on TCP there."""
        out = self._host.plugin_config("stream_output") or {}
        dynamic_ok = out.get("bitrate_mbps") != -1 or pid in self._udp_dynamic
        if path == "/v1/video.h264" and route.kind == "wifi" and self._udp and dynamic_ok:
            return "/v1/video.udp"
        return path

    def _video_path(self) -> str:
        """The phone serves each format on its own route, and runs whichever one was opened last."""
        out = self._host.plugin_config("stream_output") or {}
        return "/v1/video.h264" if out.get("format") == "h264" and h264_reader.available() else "/v1/video"

    def _fix_actions(self, res: Resolution, pid: str) -> list:
        """The banner buttons for a phone Start couldn't reach (or a dropped stream that won't come back)."""
        again = self._host.start_again(pid)
        if res.status == NOT_PAIRED:
            return [BannerAction(tr("Add phone"), self.open_add_phone)]
        update = self._outdated_action(res)
        if update is not None:
            label = tr("Update this app") if update == "desktop" else tr("Update over USB")
            return [BannerAction(label, self._update_action)]
        actions = []
        if res.status == USB_NEEDS_ATTENTION or (self._route_pref != ROUTE_AUTO and res.status == UNREACHABLE):
            actions.append(BannerAction(tr("Switch to Automatic"), lambda: self._switch_to_automatic(again)))
        if res.status in (UNREACHABLE, LOCAL_ONLY, USB_NEEDS_ATTENTION):
            actions.append(BannerAction(tr("Try again"), again))
        return actions

    def _switch_to_automatic(self, start):
        self.set_route_preference(ROUTE_AUTO)
        start()

    def _ensure_virtual_camera(self, interactive: bool = True) -> bool:
        if v4l2_devices_ready():
            return True
        if v4l2_module_loaded():
            command = "sudo modprobe -r v4l2loopback"
            self._host.show_issue("start", Issue(
                tr("Virtual camera is set up differently"),
                tr("v4l2loopback is loaded, but without {device}: another app set it up with other settings. Close "
                   "that app and run this to hand it over to Telescope:", device=V4L2_PHONE_DEV),
                [copy_action(command)], details=command))
            return False
        if not interactive:
            # Started by itself (nobody at the keyboard asked): no password prompt out of nowhere.
            self._host.show_issue("start", Issue(
                tr("The virtual camera is off"), tr("Switching it on asks for your password."),
                [BannerAction(tr("Switch on"), self._host.start_again())], kind="warn"))
            return False
        persist = self._ask_virtual_camera()
        if persist is None:
            return False
        result = run_off_ui_thread(v4l2_setup, persist)
        if result.ok:
            return True
        if result.command:
            self._host.show_issue("start", Issue(
                tr("Set up the virtual camera"), result.message,
                [copy_action(result.command)], kind="warn", details=result.command))
        elif result.message != CANCELLED:
            self._host.show_issue("start", Issue(tr("Couldn't set up the virtual camera"), result.message,
                                                 [BannerAction(tr("Try again"), self._host.start_again())]))
        return False

    def _ask_virtual_camera(self) -> Optional[bool]:
        """Consent to the password prompt. True/False: also switch it on at startup; None: cancelled."""
        box = QMessageBox(self._host)
        box.setWindowTitle(tr("Set up the virtual camera"))
        box.setText(tr("Telescope needs to switch on its virtual camera first. This asks for your password."))
        check = Switch(tr("Also switch it on at every startup"))
        check.setChecked(True)
        box.setCheckBox(check)
        box.setStandardButtons(QMessageBox.StandardButton.Ok | QMessageBox.StandardButton.Cancel)
        box.setDefaultButton(QMessageBox.StandardButton.Ok)
        if box.exec() != QMessageBox.StandardButton.Ok:
            return None
        return check.isChecked()

    def session_target(self, pid: Optional[str] = None) -> SessionTarget:
        """How to reach phone pid's session port (the selected phone by default)."""
        selected = pid is None or pid == self._selected_id
        phone = self.phone(self._selected_id if pid is None else pid)
        route = self._holds.get(self._selected_id if pid is None else pid, (None, None))[0]
        if route is None and selected and self._resolution:
            route = self._resolution.route
        return SessionTarget(phone.auth if phone else None, route)

    @contextlib.contextmanager
    def session_channel(self, target: SessionTarget):
        """Yields a client for the phone's session port along target.route, or None if there's no route."""
        route = target.route
        if target.auth is None or route is None:
            yield None
            return
        if route.kind == "wifi":
            yield PhoneSessionClient(f"https://{route.host}:{PING_PORT}", target.auth)
            return
        local = self._tunnels.acquire(route.serial, PING_PORT)
        if local is None:
            yield None
            return
        try:
            yield PhoneSessionClient(f"https://127.0.0.1:{local}", target.auth)
        finally:
            self._tunnels.release(route.serial, PING_PORT)

    def ensure_phone_streaming(self, on_progress=None, target: Optional[SessionTarget] = None,
                               opening: Optional[dict] = None) -> tuple:
        """Start the phone's camera if it isn't running (at opening's size and rate); blocking, worker threads only."""
        target = target or self.session_target()
        with self.session_channel(target) as client:
            if client is None:
                return False, tr("Lost the connection to the phone. Try again.")
            ping = client.ping()
            if ping.status == "not_paired":
                return False, tr("The phone doesn't recognise this computer anymore.\n\n"
                                 "Click Add phone to pair it again.")
            if ping.status != "paired":
                return False, tr("Couldn't reach the phone. Open Telescope on it and try again.")
            if ping.streaming_for:
                return False, busy_other_reason(ping.streaming_for)
            if ping.streaming:
                return True, ""
            if not ping.busy:
                if on_progress:
                    on_progress(tr("Starting the phone's camera..."))
                result = client.start(opening)
                if not result.ok:
                    return False, self._start_refused_reason(result.error, result.computer)
            return self._await_streaming(client, on_progress)

    @staticmethod
    def _await_streaming(client: PhoneSessionClient, on_progress=None) -> tuple:
        """Poll until the phone reports a live stream (the start request returns before the camera is up)."""
        deadline = time.monotonic() + START_TIMEOUT
        wait_start = time.monotonic()
        started = False
        unreachable_streak = 0
        elapsed = 0.0
        while time.monotonic() < deadline:
            fast = elapsed < START_FAST_POLL_FOR and not unreachable_streak  # a quiet phone gets the slow pace
            time.sleep(START_FAST_POLL_INTERVAL if fast else START_POLL_INTERVAL)
            ping = client.ping()
            elapsed = time.monotonic() - wait_start
            if ping.streaming:
                return True, ""
            if ping.status == "not_paired":
                return False, tr("Lost contact with the phone while its camera was starting.")
            if ping.streaming_for:  # another computer's start got there first
                return False, busy_other_reason(ping.streaming_for)
            if ping.status != "paired":
                unreachable_streak += 1  # tolerate brief blips; bail on sustained failure
                if unreachable_streak >= _UNREACHABLE_STREAK_LIMIT:
                    return False, tr("Lost contact with the phone while its camera was starting.")
                if on_progress:
                    on_progress(tr("Phone went quiet for a moment, still waiting... ({seconds}s)",
                                   seconds=f"{elapsed:.0f}"))
                continue
            unreachable_streak = 0
            if ping.busy:
                started = True
            elif started:
                return False, tr("The phone's camera stopped before it finished starting.\n\n"
                                 "Check the phone for a permission prompt or an error.")
            if on_progress:
                seconds = f"{elapsed:.0f}"
                on_progress(tr("Phone's camera is opening... ({seconds}s)", seconds=seconds) if started
                            else tr("Waiting for the phone's camera... ({seconds}s)", seconds=seconds))
        return False, tr("The phone's camera didn't finish starting in time.\n\n"
                         "Try again. If it keeps happening, open Telescope on the phone.")

    def stop_phone_streaming(self, target: Optional[SessionTarget] = None):
        """Tell the phone to stop its camera (best effort); blocking, worker threads only."""
        with self.session_channel(target or self.session_target()) as client:
            if client is not None:
                client.stop()

    @staticmethod
    def _start_refused_reason(error: Optional[str], computer: Optional[str] = None) -> str:
        if error == "busy_other":
            return busy_other_reason(computer)
        return {
            "no_camera_permission": tr("The phone hasn't given Telescope camera access.\n\n"
                                       "Open the app on the phone and allow the camera permission."),
            "busy": tr("The phone is busy starting or stopping a stream.\n\nTry again in a moment."),
            "start_refused": tr("Android didn't let the camera start in the background.\n\n"
                                "Bring Telescope to the front on the phone and try again."),
            "not_paired": tr("The phone doesn't recognise this computer anymore.\n\n"
                             "Click Add phone to pair it again."),
        }.get(error or "") or tr("The phone refused to start streaming ({error}).", error=error or tr("unknown error"))

    def on_stream_start(self, stream_url: str, ctrl):
        self._transport_udp = stream_url.endswith(".udp")
        if not self._streaming:  # also called after an auto-reconnect; that isn't a fresh start
            self._connected = False
        self._streaming = True
        self._lost = False
        self._switch_usb_row.setVisible(False)
        self._usb_watch_timer.stop()
        self._watch_id += 1  # a USB check still running is about the route before this one
        if (self._stream_route is not None and self._stream_route.kind == "wifi"
                and self._route_pref == ROUTE_AUTO):
            self._usb_watch_timer.start(_USB_WATCH_MS)
        self._render()

    def on_stream_stop(self):
        self._streaming = False
        self._usb_watch_timer.stop()
        self._watch_id += 1
        self._switch_usb_row.setVisible(False)
        for pid in list(self._holds):
            if not self._host.is_streaming_from(pid):  # the panels may just have moved on to another stream
                self.release_stream(pid)
        self._lost = False
        self._render()
        self._check_status()

    def _shows(self, sid: str) -> bool:
        """Whether sid's stream is the one this card shows (a bus signal can be about one behind it)."""
        return sid == (self._host.focused_source_id() or "")

    def _on_stream_connected(self, sid: str):
        if not self._shows(sid):
            return
        self._connected = True
        self._lost = False
        self._render()

    def _on_stream_lost(self, sid: str):
        if not self._shows(sid):
            return
        self._lost = True
        if self._stream_route is not None and self._stream_route.kind == "usb":
            self._usb_sat_out = time.monotonic()  # so Wi-Fi recovery doesn't hand straight back to a flaky cable
        # Recovery picks USB by itself when the phone answers on it, so no Switch to USB meanwhile.
        self._usb_watch_timer.stop()
        self._watch_id += 1
        self._switch_usb_row.setVisible(False)
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
            res = resolver.resolve(probe, ROUTE_USB)
            try:
                signals.usb_available.emit(watch_id, res.route if res.status == READY else None)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_usb_available(self, watch_id: int, route):
        if watch_id != self._watch_id or not self._streaming:
            return
        if route is None:
            self._switch_usb_row.setVisible(False)
            return
        if self._switch_usb_row.isVisible() or time.monotonic() - self._usb_sat_out < _USB_SIT_OUT_S:
            return  # it just failed: the button's there to try by hand
        # Opens next to the Wi-Fi stream and moves once it's healthy; Wi-Fi only closes then
        if not self._host.hand_over_stream(route, self._selected_id):
            self._switch_usb_row.setVisible(True)

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
        streams = existing is not None and self._host.is_streaming_from(existing.id)
        if existing:
            existing.token = result.token  # re-pairing replaced this computer's token on the phone
            existing.cert_sha256 = result.cert_sha256
            existing.ips = list(result.ips)
            existing.active_ip = active
        else:
            phone = Phone(result.phone_id, result.name, result.token, list(result.ips), active, result.cert_sha256)
            old = self._earlier_install(phone)
            if old is not None and self._ask_same_phone(old.name):
                self._take_over(old, phone)
            else:
                self._phones.append(phone)
        self._select(result.phone_id, force=True)  # one streaming next to others gets the panels
        if self._no_phone_issue is not None:  # a Start pressed before there was a phone left it up
            self._host.clear_issue("start", self._no_phone_issue)
            self._no_phone_issue = None
        if streams:
            self._host.reconnect_stream(result.phone_id)  # the running stream still holds the old token
        self._host.save_now()
        if self._phones_dlg is not None and self._phones_dlg.isVisible():
            self._phones_dlg.refresh()

    def _earlier_install(self, phone: Phone) -> Optional[Phone]:
        """A listed phone with phone's name that no longer answers as itself: maybe the same phone after Telescope
        was reinstalled on it or its data cleared, which gives it a new id."""
        for old in self._phones:
            if old.id == phone.id or old.name != phone.name or self._host.is_streaming_from(old.id):
                continue
            if old.id == self._selected_id and self._resolution is not None:
                status = self._resolution.status  # its last check
            else:
                try:
                    status = run_off_ui_thread(self._resolver.resolve, Phone(**old.to_dict()), ROUTE_AUTO).status
                except Exception:
                    logger.exception("Checking %s failed", old.name)
                    continue
            if status in (NOT_PAIRED, UNREACHABLE, USB_NEEDS_ATTENTION):
                return old
        return None

    def _ask_same_phone(self, name: str) -> bool:
        parent = self._add_dlg if self._add_dlg is not None and self._add_dlg.isVisible() else self._host
        box = QMessageBox(parent)
        box.setWindowTitle(tr("Same phone?"))
        box.setText(tr("Is this {name} again?", name=name))
        box.setInformativeText(tr(
            "A phone called {name} paired as a new phone. That happens after Telescope is reinstalled on it or its "
            "data is cleared. Yes moves {name}'s settings and presets over to it and removes the old entry.",
            name=name))
        box.setStandardButtons(QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No)
        box.setDefaultButton(QMessageBox.StandardButton.Yes)
        return box.exec() == QMessageBox.StandardButton.Yes

    def _take_over(self, old: Phone, phone: Phone):
        """phone is old again with a new id: it takes old's place, settings and virtual camera."""
        logger.info("%s paired with a new id; moving its settings over", old.name)
        self._host.move_device_settings(old.id, phone.id)
        self._phones[self._phones.index(old)] = phone
        if self._active_key == old.id:
            self._active_key = phone.id  # the panels hold its settings already, now the new id's
        self.release_stream(old.id)
        self._paths.pop(old.id, None)

    def rename_phone(self, pid: str, name: str):
        phone = self.phone(pid)
        if phone:
            phone.name = clean_name(name) or phone.name
            self._refresh_combo()
            self._host.save_now()
            self._render()

    def forget_phone(self, pid: str):
        phone = self.phone(pid)
        if phone is None:
            return
        if pid == self._selected_id and self._streaming:
            self._host.stop_stream()  # while it's still paired, so the stop can reach the phone's camera
        elif self._host.is_streaming_from(pid):
            self._host.stop_stream(pid)
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
            with self.session_channel(SessionTarget(phone.auth, res.route)) as client:
                if client is not None:
                    client.unpair()
        threading.Thread(target=revoke, daemon=True).start()

    def _select(self, pid: Optional[str], force: bool = False, switch: bool = False):
        """switch: picked by hand mid-stream, so the stream moves to pid instead of just stopping."""
        if pid == self._selected_id and not force:
            return
        if pid != self._selected_id and self._host.pick_source(pid):
            self._refresh_combo()  # the host took it from here (it streams already, or replaces one of several)
            return
        restart = False
        if self._streaming and pid != self._selected_id:
            self._host.stop_stream()
            restart = switch
        self._mark_selected(pid)
        self._activate_profile(pid)
        self._show_selection()
        if restart:
            self._host.start_stream()

    def select(self, pid: Optional[str]):
        """Pick pid, as the picker would (its settings come in; whatever streamed stops)."""
        self._select(pid)

    def show_selected(self, pid: Optional[str]):
        """The host moved the panels to pid's stream and swapped the settings itself: show it as picked."""
        if pid == self._selected_id:
            return
        self._mark_selected(pid)
        self._active_key = pid
        self._show_selection()

    def _mark_selected(self, pid: Optional[str]):
        self._selected_id = pid
        self._resolution = None
        self._update_note = None
        self._refresh_combo()

    def _show_selection(self):
        self._render()
        self._announce_source()
        self._running_check = None  # the other phone's check is stale now; don't wait for it
        self._check_status()

    def _on_combo_changed(self, idx: int):
        if self._switching:
            return
        pid = self._phone_combo.itemData(idx)
        if pid:
            self._select(pid, switch=True)

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

    def diagnostics(self) -> dict:
        res = self._resolution
        route = self._stream_route if self._streaming else (res.route if res else None)
        out = {
            "Paired phones": str(len(self._phones)),
            "Streaming from": "a phone" if self._source_name(self._selected_id) is None else self._selected_id,
            "Connect via": self._route_pref,
            "Connection": route.kind if route else "none",
            "Phone status": res.status if res else "not checked",
        }
        if res and res.usb_note:
            out["USB"] = res.usb_note
        if res and res.phone_version:
            out["Phone app"] = res.phone_version
        return out

    def get_config(self) -> dict:
        return {
            "computer_id": self._computer_id,
            "computer_name": self._computer_name,
            "route": self._route_pref,
            "udp": self._udp,
            "phones": [p.to_dict() for p in self._phones],
            "selected_phone": self._selected_id,
        }

    def set_config(self, cfg: dict):
        cid = cfg.get("computer_id")
        if isinstance(cid, str) and cid:
            self._computer_id = cid
        name = cfg.get("computer_name")
        if isinstance(name, str) and clean_name(name):
            self._computer_name = clean_name(name)
        route = cfg.get("route")
        self._route_pref = route if route in (ROUTE_AUTO, ROUTE_USB, ROUTE_WIFI) else ROUTE_AUTO
        self._udp = cfg.get("udp") is True
        if self._udp_cb is not None:
            self._show_udp()
        self._phones = []
        raw_phones = cfg.get("phones")
        for raw in raw_phones if isinstance(raw_phones, list) else []:
            try:
                self._phones.append(Phone.from_dict(raw))
            except ValueError:
                logger.warning("Discarding malformed phone entry in config: %r", raw)
        sel = cfg.get("selected_phone")
        known = self.phone(sel) or self._source_name(sel)
        self._selected_id = sel if known else (self._phones[0].id if self._phones else None)
        self._resolution = None
        self._refresh_combo()
        self._render()
        self._announce_source()
        self._check_status()

    def shutdown(self):
        self._discovery.stop()
        stop_adb_server()
