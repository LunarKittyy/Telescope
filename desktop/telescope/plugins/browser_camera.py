"""Browser camera: stream from any device with a browser, no Telescope app on it (see telescope/browser_server.py).

"Browser camera" in the phone picker runs the server and shows this card with the code to scan. Each browser that
connects is a StreamSource of its own, listed in the picker by its device while it's there, and starts streaming by
itself the way opening the app on a phone would: the first as the stream, the next ones next to it, each to a virtual
camera of its own. The server stops once Browser camera isn't picked and no browser streams.

A browser's settings are kept like a phone's, but only once something was changed from the defaults (the host drops
the rest), so scanning once from someone's phone leaves nothing behind. Remembered browsers are listed with the
phones, where they can be removed, and are forgotten after REMEMBER_DAYS without connecting.
"""

import logging
import time
from typing import Optional

from PyQt6.QtCore import QObject, QTimer, pyqtSignal
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import QHBoxLayout, QPushButton, QVBoxLayout, QWidget

from telescope import ip_utils, vcam
from telescope.browser_server import (
    BrowserControl, BrowserFeed, BrowserHub, BrowserReader, BrowserServer, certificate_available, ensure_certificate,
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
BROWSER_PREFIX = "browser:"  # then the id the browser's page keeps
REMEMBER_DAYS = 60
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


def is_browser(source_id: Optional[str]) -> bool:
    return bool(source_id) and (source_id == SOURCE_ID or source_id.startswith(BROWSER_PREFIX))


class _Source:
    """Browser camera in the picker: Start streams a browser that's there, or waits for one to connect."""

    id = SOURCE_ID
    name = "Browser camera"
    url = "browser:"
    remember_changes_only = True  # nothing streams as it, so there's nothing to keep

    def __init__(self, plugin: "BrowserCameraPlugin"):
        self._plugin = plugin

    def redirect(self) -> Optional[str]:
        return self._plugin.idle_browser()

    def prepare(self, interactive: bool) -> bool:
        return self._plugin.prepare()

    # Never streams itself (prepare() says to scan the code instead), so these read a feed nothing connects to
    def open_reader(self):
        return BrowserReader(BrowserFeed())

    def control_client(self):
        return BrowserControl(BrowserFeed())

    def fps(self) -> int:
        return self._plugin.fps


class _BrowserSource:
    """One connected browser."""

    url = "browser:"
    remember_changes_only = True

    def __init__(self, plugin: "BrowserCameraPlugin", bid: str, feed):
        self._plugin = plugin
        self.bid, self.feed = bid, feed
        self.id = BROWSER_PREFIX + bid
        self.name = "Browser"

    def prepare(self, interactive: bool) -> bool:
        return self._plugin.prepare(need_browser=False)

    def open_reader(self):
        return BrowserReader(self.feed)

    def control_client(self):
        return BrowserControl(self.feed)

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
        self.hub = BrowserHub(can_join=self._can_join)
        self._room = vcam.MAX_SLOTS  # browsers that may still join: the GUI thread sets it, the server's reads it
        self._browsers: dict = {}  # id the page keeps: its _BrowserSource, while it's there or streams
        self._started: dict = {}  # id: the connection that started streaming, so a stop isn't undone right away
        self._joining = False
        self._waiting = False  # Start or + asked for a browser and none has joined yet: the card stays up for its code
        self._known: dict = {}  # id: {"name", "seen"} of browsers that connected, for the phones list
        self._server: Optional[BrowserServer] = None
        self._problem = ""  # why the server isn't running
        self._selected_id = ""  # the picked source, when it's Browser camera or a browser
        self._size, self.fps = DEFAULT_SIZE, DEFAULT_FPS
        self._address = ""  # the address picked for the code, if there's more than one
        self._card: Optional[QWidget] = None
        self._signals = _Signals()
        self._signals.changed.connect(self._on_server_changed)
        bus.source_selected.connect(self._on_source_selected)
        bus.stream_behind.connect(self._on_stream_behind)
        bus.streams_changed.connect(self._on_streams_changed)
        bus.forget_source_requested.connect(self.forget_browser)
        host.add_stream_source(_Source(self))
        QTimer.singleShot(0, self._tidy_known)  # once the saved list is in

    @property
    def _selected(self) -> bool:
        return bool(self._selected_id)

    @property
    def _needed(self) -> bool:
        """Whether the card shows and the server runs even with no browser streaming."""
        return self._selected or self._waiting

    def _shown_feed(self):
        """The feed of the browser the panels show, if they show one."""
        src = self._browsers.get(self._selected_id[len(BROWSER_PREFIX):]) if self._selected_id else None
        return src.feed if src is not None else None

    def _connected(self) -> list:
        return [src for src in self._browsers.values() if src.feed.connected]

    # ── UI ────────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = self._card = create_card()
        lay = card_layout(card)
        self._new_link_btn = card_action("New link", "reset", "Make a new code; the old one stops working")
        self._new_link_btn.clicked.connect(self.new_link)
        add_card_header(lay, "Browser camera", "qr", action=self._new_link_btn)

        self._status_lbl = ElidingLabel("")
        lay.addLayout(control_row("Browser", self._status_lbl, stretch=True))
        self._list_lbl = wrapped_note("")  # each connected browser and the camera it streams to
        self._list_row = control_row_widget("", self._list_lbl, stretch=True)
        lay.addWidget(self._list_row)
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
        if card is not None and card.parentWidget() is not None and card.isHidden() == self._needed:
            card.setVisible(self._needed)

    def _url(self) -> str:
        server = self._server
        return server.url_for(self._address) if server and self._address else ""

    def _render(self):
        if self._card is None:
            return
        server, feed = self._server, self._shown_feed()
        connected = self._connected() if server is not None else []
        tip = ""
        if server is None:
            kind, text = ("status_err", self._problem) if self._problem else ("status_dim", "Off")
        elif feed is not None and feed.connected:
            codec = _CODEC_NAMES.get(feed.codec)
            kind, text = "status_ok", f"● {feed.device or 'Connected'}" + (f", {codec}" if codec else "")
            if feed.codec_note:
                tip = f"{text}\n{feed.codec_note}"  # why it isn't H.264
        elif connected:
            kind, text = "status_ok", f"● {len(connected)} connected" if len(connected) > 1 else f"● {connected[0].name}"
        else:
            kind, text = "status_dim", "Waiting for a browser"
        set_status_kind(self._status_lbl, kind)
        self._status_lbl.setText(text)
        if tip:
            self._status_lbl.setToolTip(tip)
        lines = [f"{src.name}: {self._host.stream_output(src.id) or 'not streaming'}" for src in connected]
        self._list_lbl.setText("\n".join(lines))
        self._list_row.setVisible(len(lines) > 1)
        mic = feed.mic_error if server is not None and feed is not None and feed.connected else ""
        self._mic_lbl.setText(f"No microphone: {mic}" if mic else "")
        self._mic_row.setVisible(bool(mic))
        hint = waiting_hint(server.stats if server else None, bool(connected), time.monotonic())
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
        sid = sid if is_browser(sid) else ""
        if sid == self._selected_id:
            return
        was = self._selected
        self._selected_id = sid
        if sid:
            self._start_server()
        elif was and not self._waiting and not self._any_streaming():  # one still streaming keeps going while the panels move on
            self._stop_server()
        self._show_card()
        self._render()

    def _any_streaming(self) -> bool:
        return any(self._host.is_streaming_from(src.id) for src in self._browsers.values())

    def _on_streams_changed(self, _count: int):
        for src in self._browsers.values():
            src.feed.set_camera(self._host.stream_output(src.id))
        self._update_room()
        if not self._needed and not self._any_streaming():
            self._stop_server()
        else:
            self._tidy_browsers()
            self._join_waiting()
        self._note_remembered()
        self._render()

    def _update_room(self):
        waiting = sum(1 for src in self._connected() if not self._host.is_streaming_from(src.id))
        self._room = vcam.MAX_SLOTS - self._host.stream_count() - waiting

    def _can_join(self, _bid: str) -> bool:
        """Server thread: whether a browser that isn't known yet gets a feed (there's a camera left for it)."""
        if self._room <= 0:
            return False
        self._room -= 1  # taken until the GUI thread counts again
        return True

    def idle_browser(self) -> Optional[str]:
        """A connected browser that doesn't stream, for Start with Browser camera picked."""
        return next((src.id for src in self._connected() if not self._host.is_streaming_from(src.id)), None)

    def _tidy_browsers(self):
        """Offer each connected browser as a source (by its device, once it says), and take back the ones that left
        and don't stream."""
        for bid, feed in self.hub.feeds().items():
            src = self._browsers.get(bid)
            if feed.connected:
                name = feed.device or "Browser"
                if src is None:
                    src = self._browsers[bid] = _BrowserSource(self, bid, feed)
                    src.name = name
                    self._host.add_stream_source(src)
                elif src.name != name:
                    src.name = name
                    self._host.add_stream_source(src)
                self._note_seen(bid, name)
            elif src is not None and not self._host.is_streaming_from(src.id):
                self._drop_browser(bid)

    def _drop_browser(self, bid: str):
        src = self._browsers.pop(bid, None)
        self._started.pop(bid, None)
        self.hub.drop(bid)
        if src is not None:
            self._host.remove_stream_source(src.id)

    def _join_waiting(self):
        """A browser that just connected starts streaming, the way opening the app on a phone would."""
        if self._joining or self._host.is_starting():
            return
        self._joining = True
        try:
            for bid, src in list(self._browsers.items()):
                feed = src.feed
                if not feed.connected or self._started.get(bid) == feed.generation:
                    continue
                if self._host.is_streaming_from(src.id):
                    self._started[bid] = feed.generation
                    continue
                if self._host.stream_count() >= vcam.MAX_SLOTS:
                    break
                self._started[bid] = feed.generation
                self._host.clear_issue("start")
                self._set_waiting(False)
                self._host.stream_source(src.id)
                break  # one at a time: the next once this one is through (streams_changed)
        finally:
            self._joining = False
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
            self._update_room()
            server = self._server_cls(self.hub, cert, key, on_change=self._signals.changed.emit)
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
        for bid in list(self._browsers):
            if not self._host.is_streaming_from(self._browsers[bid].id):
                self._drop_browser(bid)
        for bid in list(self.hub.feeds()):
            self.hub.drop(bid)

    def new_link(self):
        if self._server is not None:
            self._server.new_token()
            self._render()

    def prepare(self, need_browser: bool = True) -> bool:
        """At Start: the server has to be up, or a browser has nowhere to connect. With Browser camera picked and no
        browser there yet, nothing starts: the first browser to connect does."""
        if not self._start_server():
            self._render()
            self._host.show_issue("start", Issue("Browser camera isn't available", self._problem,
                                                 [BannerAction("Try again", self._host.start_again())]))
            return False
        if need_browser:
            self._set_waiting(True)
            self._host.show_issue("start", Issue(
                "Waiting for a browser", "Scan the code on the Browser camera card with the device to use. It "
                "starts streaming as soon as it connects.", [BannerAction("Cancel", self.cancel_waiting)],
                kind="warn", on_dismiss=self.cancel_waiting))
            return False
        return True

    def _set_waiting(self, waiting: bool):
        if waiting != self._waiting:
            self._waiting = waiting
            self._show_card()
            self._render()

    def cancel_waiting(self):
        """Banner's Cancel: nobody's coming, so the card goes unless Browser camera is picked."""
        self._host.clear_issue("start")
        self._set_waiting(False)
        if not self._needed and not self._any_streaming():
            self._stop_server()
            self._render()

    def _on_server_changed(self):
        if self._server is None:
            return
        self._update_room()
        self._tidy_browsers()
        self._join_waiting()
        self._render()

    # ── Capture settings ──────────────────────────────────────────────────

    def _set_capture(self, size: Optional[str] = None, fps: Optional[int] = None):
        if size is not None:
            self._size = size
        if fps is not None and fps != self.fps:
            self.fps = fps
            if self._host.is_streaming_from(self._selected_id):
                self._host.update_stream_output(fps=fps)  # the shown stream; the others take it when they restart
        self._apply_capture()
        self._host.schedule_save()

    def _apply_capture(self):
        _, w, h = next((s for s in SIZES if s[0] == self._size), SIZES[1])
        self.hub.set_capture(w, h, self.fps)

    # ── Remembered browsers ───────────────────────────────────────────────

    def _note_seen(self, bid: str, name: str):
        entry = self._known.get(bid)
        now = time.time()
        if entry is None or entry["name"] != name or now - entry["seen"] > 3600:
            self._known[bid] = {"name": name, "seen": now}
            self._host.schedule_save()

    def _tidy_known(self):
        """Forget browsers that haven't connected for REMEMBER_DAYS, and ones that never had a setting changed."""
        cutoff = time.time() - REMEMBER_DAYS * 86400
        for bid, entry in list(self._known.items()):
            sid = BROWSER_PREFIX + bid
            if bid in self._browsers:
                continue
            if entry["seen"] < cutoff:
                self._host.forget_device_settings(sid)
            elif self._host.has_device_settings(sid):
                continue
            del self._known[bid]
        self._host.schedule_save()
        self._note_remembered()

    def remembered(self) -> list:
        """[(id, name, detail)] of browsers with settings of their own."""
        out = []
        for bid, entry in sorted(self._known.items(), key=lambda kv: -kv[1]["seen"]):
            sid = BROWSER_PREFIX + bid
            if self._host.has_device_settings(sid):
                out.append((sid, entry["name"], "Browser, last connected " + time.strftime(
                    "%Y-%m-%d", time.localtime(entry["seen"]))))
        return out

    def _note_remembered(self):
        self._bus.remembered_sources.emit(self.remembered())

    def forget_browser(self, sid: str):
        if not sid.startswith(BROWSER_PREFIX):
            return
        bid = sid[len(BROWSER_PREFIX):]
        if self._host.is_streaming_from(sid):
            self._host.stop_stream(sid)
        self._host.forget_device_settings(sid)
        self._known.pop(bid, None)
        self._host.schedule_save()
        self._note_remembered()

    # ── Plugin hooks ──────────────────────────────────────────────────────

    def diagnostics(self) -> dict:
        server = self._server
        if server is None:
            return {"Browser camera": "off" if not self._problem else "not available"}
        stats = server.stats
        port = f"{stats.port}" + (f" ({stats.wanted_port} was taken)" if stats.fell_back else "")
        sending = [src.feed.codec or "nothing yet" for src in self._connected()]
        notes = sorted({src.feed.codec_note for src in self._connected() if src.feed.codec_note})
        return {
            "Browser camera": (f"{len(sending)} browser{'s' if len(sending) > 1 else ''} connected, sending "
                               f"{', '.join(sending)}" + (f" ({'; '.join(notes)})" if notes else "")
                               if sending else "waiting for a browser"),
            "Browser camera port": port,
            # How far browsers got: none at all points at the network or firewall, handshakes alone at the certificate
            "Browser camera reached": (f"{stats['connections']} connections, {stats['tls_failed']} certificate "
                                       f"refusals, {stats['pages']} page loads, {stats['refused']} old links, "
                                       f"{stats['browsers']} browsers"),
        }

    def get_config(self) -> dict:
        return {"size": self._size, "fps": self.fps, "address": self._address, "known": dict(self._known)}

    def set_config(self, cfg: dict):
        size = cfg.get("size")
        self._size = size if size in [s[0] for s in SIZES] else DEFAULT_SIZE
        fps = cfg.get("fps")
        self.fps = fps if fps in FPS_CHOICES else DEFAULT_FPS
        address = cfg.get("address")
        self._address = address if isinstance(address, str) else ""
        known = cfg.get("known")
        self._known = {bid: {"name": e["name"], "seen": float(e["seen"])} for bid, e in known.items()
                       if isinstance(e, dict) and isinstance(e.get("name"), str)
                       and isinstance(e.get("seen"), (int, float))} if isinstance(known, dict) else {}
        if self._card is not None:
            self._sync_combos()
        if self._server is not None:
            self._fill_addresses()
        self._apply_capture()
        self._render()

    def shutdown(self):
        self._stop_server()
