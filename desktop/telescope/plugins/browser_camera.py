"""Browser camera: stream from any device with a browser, no Telescope app on it (see telescope/browser_server.py).

Offered as a StreamSource, so it's an entry in the phone picker. While it's picked the server runs and this card
shows the code to scan; picking a phone stops the server. A browser connecting starts the stream by itself, the
way opening the app on a phone would.
"""

import logging
import time
from typing import Optional


from PyQt6.QtCore import QObject, QTimer, pyqtSignal
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import QHBoxLayout, QPushButton, QVBoxLayout, QWidget

from telescope import ip_utils
from telescope.browser_server import (
    BrowserControl, BrowserFeed, BrowserReader, BrowserServer, certificate_available, ensure_certificate,
)
from telescope.config import config_path
from telescope.plugin import TelescopePlugin
from telescope.widgets.banner import BannerAction, Issue
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, add_card_header, card_action, card_layout, control_row, control_row_widget,
    create_card, set_status_kind, wrapped_note,
)
from telescope.widgets.qr import QRCodeWidget

logger = logging.getLogger(__name__)

SOURCE_ID = "browser"
SIZES = [("480p", 854, 480), ("720p", 1280, 720), ("1080p", 1920, 1080)]
FPS_CHOICES = [15, 24, 30]
DEFAULT_SIZE, DEFAULT_FPS = "720p", 30
_CODEC_NAMES = {"h264": "H.264", "jpeg": "JPEG"}

_CERT_NOTE = ("The browser warns that the connection isn't private: it's this computer's own certificate. "
              "On iPhone tap Show Details, then visit this website. In Chrome tap Advanced, then Proceed.")
_BACKGROUND_NOTE = "Keep the page open with the screen on: phones pause the camera when you leave the browser."
# Waiting this long with nothing reaching the server is worth a hint: scanning and loading the page take seconds.
HINT_AFTER_S = 30


def waiting_hint(stats, connected: bool, now: float) -> str:
    """What to check when no browser has connected yet, from how far browsers got. "" when there's nothing to say."""
    if connected or stats is None:
        return ""
    if stats["browsers"] or stats["pages"]:
        return ""  # one got through before, or is on the page now
    if stats["tls_failed"]:
        return "A browser reached this computer but hasn't accepted its certificate yet. Tap through the warning."
    if stats.nothing_arrived() and now - stats.started >= HINT_AFTER_S:
        return (f"Nothing has reached this computer yet. Check that the device is on the same network as the address "
                f"in the code, and that the firewall allows port {stats.port}.")
    return ""


class _Signals(QObject):
    changed = pyqtSignal()  # the server's threads: a browser connected, left or said hello


class _Source:
    """The StreamSource the host streams from while Browser camera is picked."""

    id = SOURCE_ID
    name = "Browser camera"
    url = "browser:"

    def __init__(self, plugin: "BrowserCameraPlugin"):
        self._plugin = plugin

    def prepare(self, interactive: bool) -> bool:
        return self._plugin.prepare()

    def open_reader(self):
        return BrowserReader(self._plugin.feed)

    def control_client(self):
        return BrowserControl(self._plugin.feed)

    def fps(self) -> int:
        return self._plugin.fps


class BrowserCameraPlugin(TelescopePlugin):
    name = "browser_camera"
    panel_region = "right"  # where the Camera card is, which hides while this streams

    def __init__(self, server_cls=BrowserServer, cert_folder=None, addresses=None):
        self._server_cls = server_cls
        self._cert_folder = cert_folder  # tests pass a temporary folder
        self._addresses = addresses or ip_utils.get_pairing_addresses

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self.feed = BrowserFeed()
        self._server: Optional[BrowserServer] = None
        self._problem = ""  # why the server isn't running
        self._selected = False
        self._was_connected = False
        self._size, self.fps = DEFAULT_SIZE, DEFAULT_FPS
        self._address = ""  # the address picked for the code, if there's more than one
        self._card: Optional[QWidget] = None
        self._signals = _Signals()
        self._signals.changed.connect(self._on_server_changed)
        bus.source_selected.connect(self._on_source_selected)
        bus.stream_behind.connect(self._on_stream_behind)
        host.add_stream_source(_Source(self))

    # ── UI ────────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = self._card = create_card()
        lay = card_layout(card)
        self._new_link_btn = card_action("New link", "reset", "Make a new code; the old one stops working")
        self._new_link_btn.clicked.connect(self.new_link)
        add_card_header(lay, "Browser camera", "qr", action=self._new_link_btn)

        self._status_lbl = ElidingLabel("")
        lay.addLayout(control_row("Browser", self._status_lbl, stretch=True))
        self._mic_lbl = wrapped_note("", "status_warn")
        self._mic_row = control_row_widget("", self._mic_lbl, stretch=True)
        lay.addWidget(self._mic_row)
        self._hint_lbl = wrapped_note("", "status_warn")
        self._hint_row = control_row_widget("", self._hint_lbl, stretch=True)
        lay.addWidget(self._hint_row)
        # Looks again once the wait for a first connection has gone on long enough to be worth a hint
        self._hint_timer = QTimer(card)
        self._hint_timer.setSingleShot(True)
        self._hint_timer.timeout.connect(self._render)

        self._qr_box = QWidget()
        self._qr_lay = QVBoxLayout(self._qr_box)
        self._qr_lay.setContentsMargins(0, 4, 0, 4)
        self._qr: Optional[QRCodeWidget] = None
        self._qr_url = ""
        self._qr_row = control_row_widget("Scan", self._qr_box)
        lay.addWidget(self._qr_row)

        self._addr_combo = NoScrollComboBox()
        self._addr_combo.setToolTip("Which of this computer's addresses the code points to")
        self._addr_combo.currentIndexChanged.connect(self._on_address_picked)
        self._addr_row = control_row_widget("Address", self._addr_combo, stretch=True)
        lay.addWidget(self._addr_row)

        link = QHBoxLayout()
        link.setContentsMargins(0, 0, 0, 0)
        link.setSpacing(8)
        self._url_lbl = ElidingLabel("")
        self._copy_btn = QPushButton("Copy")
        self._copy_btn.setToolTip("Copy the link, to open it on a device that can't scan the code")
        self._copy_btn.clicked.connect(lambda: QGuiApplication.clipboard().setText(self._url()))
        link.addWidget(self._url_lbl, 1)
        link.addWidget(self._copy_btn)
        self._link_row = control_row_widget("Link", link, stretch=True)
        lay.addWidget(self._link_row)

        self._size_combo = NoScrollComboBox()
        for label, w, h in SIZES:
            self._size_combo.addItem(label, label)
        self._size_combo.setToolTip("The size the browser sends; a camera that can't do it sends what it can")
        self._size_combo.currentIndexChanged.connect(lambda i: self._set_capture(size=self._size_combo.itemData(i)))
        lay.addLayout(control_row("Resolution", self._size_combo))
        self._fps_combo = NoScrollComboBox()
        for fps in FPS_CHOICES:
            self._fps_combo.addItem(f"{fps} fps", fps)
        self._fps_combo.currentIndexChanged.connect(lambda i: self._set_capture(fps=self._fps_combo.itemData(i)))
        lay.addLayout(control_row("Frame rate", self._fps_combo))

        self._note_lbl = wrapped_note(f"{_CERT_NOTE} {_BACKGROUND_NOTE}")
        lay.addWidget(control_row_widget("", self._note_lbl, stretch=True))

        self._sync_combos()
        self._render()
        # The host places the card after this returns, and only then can it be hidden. Owned by the card, so it
        # never fires on a card that's gone.
        placed = QTimer(card)
        placed.setSingleShot(True)
        placed.timeout.connect(self._show_card)
        placed.start(0)
        return card

    def _show_card(self):
        card = self._card
        if card is not None and card.parentWidget() is not None and card.isHidden() == self._selected:
            card.setVisible(self._selected)

    def _url(self) -> str:
        server = self._server
        return server.url_for(self._address) if server and self._address else ""

    def _render(self):
        if self._card is None:
            return
        server, feed = self._server, self.feed
        if server is None:
            kind, text = ("status_err", self._problem) if self._problem else ("status_dim", "Off")
        elif feed.connected:
            codec = _CODEC_NAMES.get(feed.codec)
            kind, text = "status_ok", f"● {feed.device or 'Connected'}" + (f", {codec}" if codec else "")
        else:
            kind, text = "status_dim", "Waiting for a browser"
        set_status_kind(self._status_lbl, kind)
        self._status_lbl.setText(text)
        if server is not None and feed.connected and feed.codec_note:
            self._status_lbl.setToolTip(f"{text}\n{feed.codec_note}")  # why it isn't H.264
        mic = feed.mic_error if server is not None and feed.connected else ""
        self._mic_lbl.setText(f"No microphone: {mic}" if mic else "")
        self._mic_row.setVisible(bool(mic))
        hint = waiting_hint(server.stats if server else None, feed.connected, time.monotonic())
        self._hint_lbl.setText(hint)
        self._hint_row.setVisible(bool(hint))

        url = self._url()
        if url != self._qr_url:
            self._qr_url = url
            if self._qr is not None:
                self._qr_lay.removeWidget(self._qr)
                self._qr.deleteLater()
                self._qr = None
            if url:
                self._qr = QRCodeWidget(url, module_px=4)
                self._qr_lay.addWidget(self._qr)
        self._qr_row.setVisible(bool(url))
        self._link_row.setVisible(bool(url))
        self._url_lbl.setText(url)
        self._addr_row.setVisible(server is not None and self._addr_combo.count() > 1)
        self._new_link_btn.setEnabled(server is not None)

    def _sync_combos(self):
        for combo, value in ((self._size_combo, self._size), (self._fps_combo, self.fps)):
            combo.blockSignals(True)
            combo.setCurrentIndex(max(0, combo.findData(value)))
            combo.blockSignals(False)

    def _fill_addresses(self):
        candidates = [a.ip for a in self._addresses()]
        if self._address not in candidates:
            self._address = candidates[0] if candidates else ""
        if self._card is None:
            return
        self._addr_combo.blockSignals(True)
        self._addr_combo.clear()
        for ip in candidates:
            self._addr_combo.addItem(ip, ip)
        self._addr_combo.setCurrentIndex(self._addr_combo.findData(self._address))
        self._addr_combo.blockSignals(False)

    def _on_address_picked(self, idx: int):
        ip = self._addr_combo.itemData(idx)
        if ip and ip != self._address:
            self._address = ip
            self._host.schedule_save()
            self._render()

    # ── Server ────────────────────────────────────────────────────────────

    def _on_stream_behind(self, behind: bool):
        if not self._selected:
            return
        if not behind:
            self._host.clear_issue("behind")
            return
        text = ("Try a lower resolution or frame rate on the Browser camera card." if self._size != SIZES[0][0]
                or self.fps != FPS_CHOICES[0] else "The device's browser can't send any faster.")
        self._host.show_issue("behind", Issue("Can't keep up", text, kind="warn"))

    def _on_source_selected(self, sid: str):
        selected = sid == SOURCE_ID
        if selected == self._selected:
            return
        self._selected = selected
        if selected:
            self._start_server()
        else:
            self._stop_server()
        self._show_card()
        self._render()

    def _start_server(self) -> bool:
        if self._server is not None:
            return True
        self._problem = ""
        self._fill_addresses()
        if not certificate_available():
            self._problem = "Needs the cryptography package: pip install cryptography"
            return False
        folder = self._cert_folder or config_path().parent
        try:
            cert, key = ensure_certificate(folder, [a.ip for a in self._addresses()])
            server = self._server_cls(self.feed, cert, key, on_change=self._signals.changed.emit)
            server.start()
        except Exception as exc:
            logger.exception("Browser camera server didn't start")
            self._problem = f"Couldn't start: {exc}"
            return False
        self._server = server
        self._apply_capture()
        if self._card is not None:
            self._hint_timer.start(HINT_AFTER_S * 1000)
        return True

    def _stop_server(self):
        server, self._server = self._server, None
        if server is not None:
            server.stop()
        self._was_connected = False

    def new_link(self):
        if self._server is not None:
            self._server.new_token()
            self._render()

    def prepare(self) -> bool:
        """At Start: the server has to be up, or a browser has nowhere to connect."""
        if self._start_server():
            return True
        self._render()
        self._host.show_issue("start", Issue("Browser camera isn't available", self._problem,
                                             [BannerAction("Try again", self._host.start_stream)]))
        return False

    def _on_server_changed(self):
        connected = self.feed.connected and self._server is not None
        if connected and not self._was_connected and self._selected:
            if not self._host.is_streaming() and not self._host.is_starting():
                self._host.start_stream(interactive=False)  # like opening the app on a phone
        self._was_connected = connected
        self._render()

    # ── Capture settings ──────────────────────────────────────────────────

    def _set_capture(self, size: Optional[str] = None, fps: Optional[int] = None):
        if size is not None:
            self._size = size
        if fps is not None and fps != self.fps:
            self.fps = fps
            if self._host.is_streaming() and self._selected:
                self._host.update_stream_output(fps=fps)
        self._apply_capture()
        self._host.schedule_save()

    def _apply_capture(self):
        _, w, h = next((s for s in SIZES if s[0] == self._size), SIZES[1])
        self.feed.set_capture(w, h, self.fps)

    # ── Plugin hooks ──────────────────────────────────────────────────────

    def diagnostics(self) -> dict:
        server = self._server
        if server is None:
            return {"Browser camera": "off" if not self._problem else "not available"}
        stats = server.stats
        port = f"{stats.port}" + (f" ({stats.wanted_port} was taken)" if stats.fell_back else "")
        return {
            "Browser camera": (f"browser connected, sending {self.feed.codec or 'nothing yet'}"
                               + (f" ({self.feed.codec_note})" if self.feed.codec_note else "")
                               if self.feed.connected else "waiting for a browser"),
            "Browser camera port": port,
            # How far browsers got: none at all points at the network or firewall, handshakes alone at the certificate
            "Browser camera reached": (f"{stats['connections']} connections, {stats['tls_failed']} certificate "
                                       f"refusals, {stats['pages']} page loads, {stats['refused']} old links, "
                                       f"{stats['browsers']} browsers"),
        }

    def get_config(self) -> dict:
        return {"size": self._size, "fps": self.fps, "address": self._address}

    def set_config(self, cfg: dict):
        size = cfg.get("size")
        self._size = size if size in [s[0] for s in SIZES] else DEFAULT_SIZE
        fps = cfg.get("fps")
        self.fps = fps if fps in FPS_CHOICES else DEFAULT_FPS
        address = cfg.get("address")
        self._address = address if isinstance(address, str) else ""
        if self._card is not None:
            self._sync_combos()
        if self._server is not None:
            self._fill_addresses()
        self._apply_capture()
        self._render()

    def shutdown(self):
        self._stop_server()
