"""Webcam: stream from a webcam or capture card plugged into this computer (see telescope/webcams.py).

Each webcam is a StreamSource of its own, listed in the phone picker by its name while it's plugged in, and streams
like a phone does: Start streams the one picked, + adds another next to it, each to a virtual camera of its own,
with every effect working on it. One that's unplugged while it streams keeps its place and carries on when it's
plugged back in; one that isn't streaming just leaves the picker. The card shows while a webcam is picked and holds
its resolution and frame rate, kept per camera.
"""

import logging
from typing import Optional

from PyQt6.QtCore import QObject, QTimer, pyqtSignal
from PyQt6.QtWidgets import QWidget

from telescope import vcam, webcams
from telescope.plugin import TelescopePlugin
from telescope.widgets.banner import Issue
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, add_card_header, card_layout, control_row, create_card, set_status_kind,
)
from telescope.i18n import N_, tr

logger = logging.getLogger(__name__)

SOURCE_PREFIX = "webcam:"  # then the camera's key (webcams.Device.key)
SCAN_MS = 2000  # Linux: how often to look for webcams plugged in or out (Windows hears it from Qt)
DEFAULT_FPS = 30
FPS_CHOICES = [15, 24, 25, 30, 50, 60]  # offered when the camera doesn't say which rates it has


def is_webcam(source_id: Optional[str]) -> bool:
    return bool(source_id) and source_id.startswith(SOURCE_PREFIX)


def default_scanner():
    """What lists the webcams on this system: a callable returning [webcams.Device]."""
    return webcams.LinuxScanner().scan if vcam.IS_LINUX else webcams.scan_windows


class _Signals(QObject):
    reported = pyqtSignal(str, str, str)  # a stream's threads: (camera key, state, detail), see WebcamReader


class _WebcamSource:
    """One webcam in the picker."""

    url = "webcam:"
    auto_canvas = None  # the virtual camera takes the webcam's own size
    lost_hint = N_("Check that it's plugged in.")

    def __init__(self, plugin: "WebcamPlugin", key: str, name: str):
        self._plugin = plugin
        self.key = key
        self.id = SOURCE_PREFIX + key
        self.name = name

    def prepare(self, interactive: bool) -> bool:
        return self._plugin.prepare(self)

    def open_reader(self):
        key, plugin = self.key, self._plugin
        return webcams.WebcamReader(lambda: plugin.device(key), plugin.choice,
                                    lambda state, detail="": plugin.signals.reported.emit(key, state, detail))

    def control_client(self):
        return webcams.WebcamControl()

    def fps(self) -> int:
        device = self._plugin.device(self.key)
        choice = self._plugin.choice(device) if device is not None else None
        return int(round(choice.fps)) if choice is not None and choice.fps else self._plugin.wanted(self.key)[1]


class WebcamPlugin(TelescopePlugin):
    name = "webcam"
    title = N_("Webcams")
    about = N_("Streaming from webcams and capture cards plugged into this computer.")
    category = "streaming"
    icon = "video"
    pinned = True  # takes Camera's place while a webcam is picked
    when_off = N_("Without it webcams aren't in the phone picker and the Webcam card is gone too.")
    panel_region = "right"  # where the Camera card is, which hides while a webcam is picked

    def __init__(self, scanner=None):
        self._scanner = scanner  # tests pass their own; the real one is made at setup

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._scan = self._scanner or default_scanner()
        self._devices: dict = {}  # key: webcams.Device, as plugged in now; read by the streams' threads too
        self._sources: dict = {}  # key: _WebcamSource offered to the host
        self._states: dict = {}  # key: (state, detail) its stream last reported
        self._modes: dict = {}  # key: what it was last opened as, while it streams
        self._settings: dict = {}  # key: {"size": [w, h] or None for Auto, "fps": int}
        self._selected_key = ""
        self._card: Optional[QWidget] = None
        self.signals = _Signals()
        self.signals.reported.connect(self._on_reported)
        bus.source_selected.connect(self._on_source_selected)
        bus.streams_changed.connect(self._on_streams_changed)
        self._timer = QTimer(self.signals)
        self._timer.timeout.connect(self.rescan)
        self._qt_devices = None
        self._stopped = False
        if vcam.IS_LINUX or self._scanner is not None:
            self._timer.start(SCAN_MS)
        QTimer.singleShot(0, self._first_scan)  # after the window is up: Windows loads Qt Multimedia for it

    def _first_scan(self):
        if self._stopped:
            return
        if not vcam.IS_LINUX and self._scanner is None:
            try:
                from PyQt6.QtMultimedia import QMediaDevices
                self._qt_devices = QMediaDevices(self.signals)
                self._qt_devices.videoInputsChanged.connect(self.rescan)
            except ImportError:
                logger.warning("Qt Multimedia isn't there, so plugging webcams in or out isn't noticed")
        self.rescan()

    # ── Devices ───────────────────────────────────────────────────────────

    def device(self, key: str) -> Optional[webcams.Device]:
        """Any thread: the webcam with this key as it's plugged in now, or None."""
        return self._devices.get(key)

    def wanted(self, key: str) -> tuple:
        """(size or None for Auto, fps) picked for this camera."""
        cfg = self._settings.get(key, {})
        size = cfg.get("size")
        return (tuple(size) if size else None), cfg.get("fps", DEFAULT_FPS)

    def choice(self, device: webcams.Device) -> Optional[webcams.Choice]:
        """Any thread: what to ask device for, from what's picked for it."""
        size, fps = self.wanted(device.key)
        if not device.modes:  # nothing listed: ask for what's picked and take what comes
            w, h = size or (1280, 720)
            return webcams.Choice("RAW", w, h, float(fps))
        return webcams.choose(device.modes, size, fps)

    def rescan(self):
        try:
            found = self._scan()
        except Exception:
            logger.exception("Couldn't look for webcams")
            return
        self._devices = {d.key: d for d in found}
        for key, dev in self._devices.items():
            src = self._sources.get(key)
            if src is None:
                src = self._sources[key] = _WebcamSource(self, key, dev.name)
                self._host.add_stream_source(src)
            elif src.name != dev.name:
                src.name = dev.name
                self._host.add_stream_source(src)
            if self._states.get(key, ("",))[0] == "gone":
                self._states.pop(key)  # back: its stream opens it again on its next try
        self._drop_unplugged()
        self._render()

    def _drop_unplugged(self):
        """Webcams that are gone leave the picker, unless they stream: those wait to be plugged back in."""
        for key in list(self._sources):
            if key in self._devices:
                continue
            src = self._sources[key]
            if self._host.is_streaming_from(src.id):
                self._states[key] = ("gone", "")
                continue
            del self._sources[key]
            self._states.pop(key, None)
            self._modes.pop(key, None)
            self._host.clear_issue(f"webcam:{key}")
            self._host.remove_stream_source(src.id)

    def prepare(self, src: _WebcamSource) -> bool:
        """At Start: a webcam that was unplugged since it was picked can't stream."""
        if src.key in self._devices:
            return True
        self._host.show_issue("start", Issue(tr("{name} isn't plugged in", name=src.name),
                                             tr("Plug it in, or pick another camera."), kind="warn"))
        return False

    # ── Streams ───────────────────────────────────────────────────────────

    def _on_reported(self, key: str, state: str, detail: str):
        src = self._sources.get(key)
        if src is None:
            return
        if self._states.get(key) == (state, detail):
            return
        self._states[key] = (state, detail)
        if state == "ok":
            self._modes[key] = detail
        issue_key = f"webcam:{key}"
        if state == "busy" and self._host.is_streaming_from(src.id):
            text = (tr("{app} is using it. Close it there and it starts here.", app=detail) if detail
                    else tr("Another app may be using it. Close it there and it starts here."))
            self._host.show_issue(issue_key, Issue(tr("Can't open {name}", name=src.name), text, kind="warn"))
        else:
            self._host.clear_issue(issue_key)
        self._render()

    def _on_source_selected(self, sid: str):
        key = sid[len(SOURCE_PREFIX):] if is_webcam(sid) else ""
        if key != self._selected_key:
            self._selected_key = key
            self._fill_combos()
            self._show_card()
            self._render()

    def _on_streams_changed(self, _count: int):
        for key, src in self._sources.items():
            if not self._host.is_streaming_from(src.id) and self._states.pop(key, None) is not None:
                self._modes.pop(key, None)
                self._host.clear_issue(f"webcam:{key}")
        self._drop_unplugged()
        self._render()

    # ── UI ────────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = self._card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Webcam"), self.icon, hue=self.category)
        self._status_lbl = ElidingLabel("")
        lay.addLayout(control_row(tr("Camera"), self._status_lbl, stretch=True))

        self._size_combo = NoScrollComboBox()
        self._size_combo.setToolTip(tr("The size the webcam sends; Auto takes the largest up to 1080p that keeps the "
                                       "frame rate"))
        self._size_combo.currentIndexChanged.connect(self._on_size_picked)
        lay.addLayout(control_row(tr("Resolution"), self._size_combo))
        self._fps_combo = NoScrollComboBox()
        self._fps_combo.currentIndexChanged.connect(self._on_fps_picked)
        lay.addLayout(control_row(tr("Frame rate"), self._fps_combo))

        self._fill_combos()
        self._render()
        # Hidden once the host has placed it; owned by the card, so it never fires on a card that's gone
        placed = QTimer(card)
        placed.setSingleShot(True)
        placed.timeout.connect(self._show_card)
        placed.start(0)
        return card

    def _show_card(self):
        card = self._card
        shown = bool(self._selected_key)
        if card is not None and card.parentWidget() is not None and card.isHidden() == shown:
            card.setVisible(shown)

    def _fill_combos(self):
        if self._card is None:
            return
        key = self._selected_key
        device = self._devices.get(key)
        modes = device.modes if device is not None else ()
        size, fps = self.wanted(key)
        self._size_combo.blockSignals(True)
        self._size_combo.clear()
        self._size_combo.addItem(tr("Auto"), None)
        for w, h in webcams.sizes(modes):
            self._size_combo.addItem(f"{w}×{h}", [w, h])
        index = self._size_combo.findData(list(size)) if size else 0
        if index < 0 and size:  # saved for a camera that doesn't offer it now: still shown, so it isn't lost
            self._size_combo.addItem(f"{size[0]}×{size[1]}", list(size))
            index = self._size_combo.count() - 1
        self._size_combo.setCurrentIndex(index)
        self._size_combo.blockSignals(False)

        offered = webcams.rates(modes, size) or FPS_CHOICES[::-1]
        self._fps_combo.blockSignals(True)
        self._fps_combo.clear()
        for rate in sorted(set(offered) | {fps}, reverse=True):
            self._fps_combo.addItem(f"{rate} fps", rate)
        self._fps_combo.setCurrentIndex(max(0, self._fps_combo.findData(fps)))
        self._fps_combo.blockSignals(False)
        enabled = bool(key)
        self._size_combo.setEnabled(enabled)
        self._fps_combo.setEnabled(enabled)

    def _on_size_picked(self, index: int):
        size = self._size_combo.itemData(index)
        self._set_wanted(size=list(size) if size else None)

    def _on_fps_picked(self, index: int):
        fps = self._fps_combo.itemData(index)
        if fps:
            self._set_wanted(fps=int(fps))

    def _set_wanted(self, **changes):
        key = self._selected_key
        if not key:
            return
        cfg = dict(self._settings.get(key, {}))
        cfg.update(changes)
        if cfg == self._settings.get(key):
            return
        self._settings[key] = cfg
        self._host.schedule_save()
        if "size" in changes:
            self._fill_combos()  # the rates offered depend on the size
        src = self._sources.get(key)
        if src is not None and self._host.is_streaming_from(src.id):
            self._host.reconnect_stream(src.id)  # a new size means opening the camera again
        self._render()

    def _render(self):
        if self._card is None:
            return
        key = self._selected_key
        src = self._sources.get(key)
        state, detail = self._states.get(key, ("", ""))
        streaming = src is not None and self._host.is_streaming_from(src.id)
        if not key:
            kind, text = "status_dim", ""
        elif state == "gone" or key not in self._devices:
            kind, text = "status_warn", tr("Not plugged in")
        elif state == "busy":
            kind, text = "status_err", (tr("In use by {app}", app=detail) if detail else tr("In use by another app"))
        elif state == "silent":
            kind, text = "status_warn", tr("Not sending anything")
        elif streaming:
            kind, text = "status_ok", self._modes.get(key) or tr("Starting…")
        else:
            kind, text = "status_dim", tr("Not streaming")
        set_status_kind(self._status_lbl, kind)
        self._status_lbl.setText(text)
        self._status_lbl.setToolTip(src.name if src is not None else "")

    # ── Plugin hooks ──────────────────────────────────────────────────────

    def diagnostics(self) -> dict:
        found = [f"{d.name} ({len(d.modes)} modes)" for d in self._devices.values()]
        streams = [f"{self._sources[k].name}: {s}{' ' + d if d else ''}" for k, (s, d) in self._states.items()
                   if k in self._sources]
        out = {"Webcams": "; ".join(found) if found else "none found"}
        if streams:
            out["Webcam streams"] = "; ".join(streams)
        return out

    def get_config(self) -> dict:
        return {"cameras": {key: dict(cfg) for key, cfg in self._settings.items()}}

    def set_config(self, cfg: dict):
        cameras = cfg.get("cameras")
        self._settings = {}
        for key, entry in (cameras.items() if isinstance(cameras, dict) else ()):
            if not isinstance(key, str) or not isinstance(entry, dict):
                continue
            size, fps = entry.get("size"), entry.get("fps")
            clean = {}
            if isinstance(size, list) and len(size) == 2 and all(isinstance(v, int) and v > 0 for v in size):
                clean["size"] = size
            if isinstance(fps, int) and 0 < fps <= 240:
                clean["fps"] = fps
            self._settings[key] = clean
        self._fill_combos()
        self._render()

    def shutdown(self):
        self._stopped = True
        self._timer.stop()
