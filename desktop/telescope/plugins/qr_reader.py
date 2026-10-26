"""QR and Wi-Fi code reader: a code held up to the camera goes to the clipboard, the Wi-Fi password for a Wi-Fi one."""
import threading
import time
from dataclasses import dataclass
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import QObject, QPointF, QRectF, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QColor, QGuiApplication, QPainter, QPen, QPolygonF
from PyQt6.QtWidgets import QHBoxLayout, QPushButton, QSizePolicy, QWidget

from telescope import theme
from telescope.overlays import PreviewLayer
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action
from telescope.widgets.common import (
    Switch, add_card_header, card_layout, create_card, dim_until_paired, set_status_kind, wrapped_note,
)
from telescope.i18n import N_, tr

_EVERY_S = 0.3  # a frame is looked at this often
_WIDTH = 1280  # ...shrunk to this width first, and tried again at half that (a big code close up reads better small)
_AGAIN_S = 3.0  # the same code is copied again only after it's been out of view this long
_OUTLINE_S = 1.5  # how long the code found stays outlined on the preview
_CHECK_MS = 400  # the clipboard is read back this long after a copy (Wayland ignores one from a window in the background)


@dataclass(frozen=True)
class Wifi:
    ssid: str
    password: str
    security: str  # "WPA", "WEP", "nopass" or "" when the code doesn't say
    hidden: bool


def _fields(body: str) -> dict:
    """KEY:value pairs of a WIFI: code body, split on unescaped ; (a backslash escapes \\ ; , : and ")."""
    fields, key, value, escaped, in_key = {}, "", "", False, True
    for ch in body:
        if escaped:
            if in_key:
                key += ch
            else:
                value += ch
            escaped = False
        elif ch == "\\":
            escaped = True
        elif ch == ":" and in_key:
            in_key = False
        elif ch == ";":
            if key:
                fields.setdefault(key.upper(), value)
            key, value, in_key = "", "", True
        elif in_key:
            key += ch
        else:
            value += ch
    if key and not in_key:
        fields.setdefault(key.upper(), value)
    return fields


def parse_wifi(text: str) -> Optional[Wifi]:
    """The network in a WIFI:T:WPA;S:name;P:password;; code, or None when text isn't one."""
    if text[:5].upper() != "WIFI:":
        return None
    fields = _fields(text[5:])
    if not fields.get("S"):
        return None
    return Wifi(fields["S"], fields.get("P", ""), fields.get("T", ""), fields.get("H", "").lower() == "true")


def is_link(text: str) -> bool:
    return text.lower().startswith(("http://", "https://"))


def read_code(grey: np.ndarray, detectors) -> Optional[tuple]:
    """(text, corners 0..1) of a QR code in a grey frame, "" for one seen but not read; None for no code."""
    seen = None
    h, w = grey.shape
    for image in (grey, cv2.resize(grey, (max(1, w // 2), max(1, h // 2)), interpolation=cv2.INTER_AREA)):
        size = np.array([image.shape[1], image.shape[0]], np.float32)
        for detector in detectors:
            try:
                text, points, _straight = detector.detectAndDecode(image)
            except cv2.error:
                continue
            if points is None or not len(points):
                continue
            corners = points.reshape(-1, 2)[:4] / size
            if text:
                return text, corners
            seen = seen or ("", corners)
    return seen


class _Sig(QObject):
    found = pyqtSignal(str, object)  # the code's text, its corners (0..1 in the frame) or None


class CodeLayer(PreviewLayer):
    """Outlines the code just read on the preview for a moment."""

    def __init__(self):
        self.corners: Optional[np.ndarray] = None
        self.until = 0.0

    def paint(self, p: QPainter, rect: QRectF):
        if self.corners is None or time.monotonic() > self.until:
            return
        shape = QPolygonF([QPointF(rect.x() + u * rect.width(), rect.y() + v * rect.height()) for u, v in self.corners])
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        for colour, width in ((QColor(0, 0, 0, 140), 5), (QColor(theme.OK), 3)):
            p.setPen(QPen(colour, width))
            p.drawPolygon(shape)


class QrReaderPlugin(TelescopePlugin):
    name = "qr_reader"
    title = N_("QR code reader")
    about = N_("Copies a QR code you hold up to the camera, or the password from a Wi-Fi code.")
    when_off = N_("Without it there's no QR code card and codes in the picture are left alone.")
    optional = True
    panel_region = "left"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._on = False
        self._busy = threading.Event()  # a frame is being looked at, so the next one isn't queued behind it
        self._looked_at = 0.0
        self._sig = _Sig()
        self._sig.found.connect(self._on_found)
        self._last_text = ""
        self._last_seen = 0.0
        self._layer = CodeLayer()
        self._copied = ""  # what the last code put (or tried to put) on the clipboard
        self._copied_line = ""
        self._note = ""
        self._detectors = (cv2.QRCodeDetectorAruco(), cv2.QRCodeDetector())  # each reads some codes the other misses

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip(tr("Look for QR codes in the picture"))
        self._switch.toggled.connect(self._on_switch)
        add_card_header(lay, tr("QR codes"), "qr", action=self._switch)
        dim_until_paired(card, self._bus)
        self._status = wrapped_note("")
        self._copy_btn = QPushButton(tr("Copy"))
        self._copy_btn.setToolTip(tr("Put the last code on the clipboard again"))
        self._copy_btn.clicked.connect(self._copy_again)
        self._copy_btn.hide()
        self._check_timer = QTimer(card)  # goes with the card, so a check can't outlive it
        self._check_timer.setSingleShot(True)
        self._check_timer.setInterval(_CHECK_MS)
        self._check_timer.timeout.connect(self._check)
        row = QHBoxLayout()
        row.setSpacing(8)
        row.addWidget(self._status, 1)
        row.addWidget(self._copy_btn, 0, Qt.AlignmentFlag.AlignTop)
        lay.addLayout(row)
        self._notify_switch = Switch(tr("Notify me when a code is read"))
        self._notify_switch.setChecked(True)
        self._notify_switch.toggled.connect(lambda _on: self._host.schedule_save())
        lay.addWidget(self._notify_switch)
        self._show(tr("Hold a QR code up to the camera."))
        return card

    def create_actions(self) -> list:
        return [button_action("qr_reader.on", tr("Read QR codes"), tr("QR code reader"), self._switch)]

    def _on_switch(self, on: bool):
        self._on = on
        if on:
            self._host.add_preview_layer(self.name, self._layer)
        else:
            self._host.remove_preview_layer(self.name)
        self._show(tr("Hold a QR code up to the camera.") if on else "")
        self._host.schedule_save()

    def _show(self, text: str, kind: str = ""):
        self._status.setText(text)
        self._status.setVisible(bool(text))
        set_status_kind(self._status, kind)

    # ── Codes (UI thread) ─────────────────────────────────────────────────

    def _on_found(self, text: str, corners):
        now = time.monotonic()
        if corners is not None:
            self._layer.corners, self._layer.until = corners, now + _OUTLINE_S
            self._host.refresh_preview()
        if not text:
            return
        again = text == self._last_text and now - self._last_seen < _AGAIN_S
        self._last_text, self._last_seen = text, now
        if again or not self._on:
            return
        copied, shown, note = self.copy(text)
        self._copied, self._copied_line = copied, shown
        self._copy_btn.show()
        QGuiApplication.clipboard().setText(copied)
        self._note = note
        self._check_timer.start()

    def _check(self):
        """Whether the copy reached the clipboard: a window in the background can't put anything there on Wayland."""
        copied, shown, note = self._copied, self._copied_line, self._note
        reached = QGuiApplication.clipboard().text() == copied
        if reached:
            self._show(shown, "ok")
        else:
            self._show(tr("Found a code. Click Copy to put it on the clipboard."), "warn")
        if self._notify_switch.isChecked():
            if reached:
                self._host.send_notification(tr("QR code copied"), note, urgent=False)
            else:
                self._host.send_notification(tr("QR code found"), tr("Click Copy on the QR codes card to put it on "
                                                                     "the clipboard."), urgent=False)

    def _copy_again(self):
        QGuiApplication.clipboard().setText(self._copied)
        self._show(self._copied_line, "ok")

    @staticmethod
    def copy(text: str) -> tuple:
        """(what goes to the clipboard, what the card says, what the notification says) for a code's text."""
        wifi = parse_wifi(text)
        if wifi is not None and wifi.password:
            line = tr("Copied the password for {network}", network=wifi.ssid)
            return wifi.password, line, line
        if wifi is not None:
            line = tr("Copied the name of {network}, which has no password", network=wifi.ssid)
            return wifi.ssid, line, line
        if is_link(text):
            return text, tr("Copied {link}", link=text), text
        return text, tr("Copied the code's text"), text

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        now = time.monotonic()
        if self._on and not self._busy.is_set() and now - self._looked_at >= _EVERY_S:
            self._looked_at = now
            self._busy.set()
            h, w = frame.shape[:2]
            scale = min(1.0, _WIDTH / w)
            small = cv2.resize(frame, (round(w * scale), round(h * scale)), interpolation=cv2.INTER_AREA)
            threading.Thread(target=self._look, args=(small,), name="qr-reader", daemon=True).start()
        return frame

    def _look(self, small: np.ndarray):
        try:
            found = read_code(cv2.cvtColor(small, cv2.COLOR_BGR2GRAY), self._detectors)
            if found is not None:
                self._sig.found.emit(*found)
        finally:
            self._busy.clear()

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"on": self._switch.isChecked(), "notify": self._notify_switch.isChecked()}

    def set_config(self, cfg: dict):
        self._notify_switch.setChecked(cfg.get("notify") is not False)
        self._switch.blockSignals(True)
        self._switch.setChecked(cfg.get("on") is True)
        self._switch.blockSignals(False)
        self._on_switch(self._switch.isChecked())  # once, whether or not the switch moved

    def diagnostics(self) -> dict:
        return {"QR code reader": "on" if self._on else "off"}

    def shutdown(self):
        self._on = False
