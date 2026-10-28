"""Webcam: stream from a webcam or capture card plugged into this computer (see telescope/webcams.py).

Each webcam is a StreamSource of its own, listed in the phone picker by its name while it's plugged in, and streams
like a phone does: Start streams the one picked, + adds another next to it, each to a virtual camera of its own,
with every effect working on it. One that's unplugged while it streams keeps its place and carries on when it's
plugged back in; one that isn't streaming just leaves the picker. The card shows while a webcam is picked and holds
its resolution and frame rate, plus whichever picture settings the camera itself offers (telescope/webcam_controls.py),
all kept per camera and put back each time it's opened. A camera that's only plugged in is never written to.
"""

import logging
from typing import Optional

from PyQt6.QtCore import QObject, Qt, QTimer, pyqtSignal
from PyQt6.QtWidgets import QButtonGroup, QGridLayout, QVBoxLayout, QWidget

from telescope import vcam, webcam_controls, webcams
from telescope.plugin import TelescopePlugin
from telescope.webcam_controls import AUTO_OF, AUTOS, apply_order, default_of, local_flicker
from telescope.widgets.banner import Issue
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, NoScrollSlider, SegmentButton, add_card_header, add_section_heading, card_action,
    card_layout, control_row, control_row_widget, create_card, log_pos_to_val, segmented_row, set_status_kind,
    slider_row, val_to_log_pos, value_label,
)
from telescope.widgets.controls import SwitchTile
from telescope.i18n import N_, tr

logger = logging.getLogger(__name__)

SOURCE_PREFIX = "webcam:"  # then the camera's key (webcams.Device.key)
SCAN_MS = 2000  # Linux: how often to look for webcams plugged in or out (Windows hears it from Qt)
DEFAULT_FPS = 30
FPS_CHOICES = [15, 24, 25, 30, 50, 60]  # offered when the camera doesn't say which rates it has
_LOG_STEPS = 1000  # exposure time slider positions, spread out evenly in stops

# The card's picture settings, by section, in the order they're shown: (name, label)
_SECTIONS = (
    ("light", "sun", (
        ("exposure_auto", N_("Exposure")), ("exposure_time", N_("Exposure time")), ("gain", N_("Gain")),
        ("brightness", N_("Brightness")), ("backlight", N_("Backlight compensation")),
        ("anti_flicker", N_("Anti-flicker")), ("frame_rate_drop", N_("Slow down in the dark")))),
    ("image", "image", (
        ("white_balance_auto", N_("White balance")), ("white_balance", N_("Temperature")),
        ("saturation", N_("Saturation")), ("hue", N_("Hue")), ("contrast", N_("Contrast")), ("gamma", N_("Gamma")),
        ("sharpness", N_("Sharpness")))),
    ("focus", "focus", (("focus_auto", N_("Focus")), ("focus", N_("Focus position")))),
    ("framing", "crop", (("zoom", N_("Zoom")), ("pan", N_("Pan")), ("tilt", N_("Tilt")))),
)


def _section_title(section: str) -> str:
    return {"light": tr("Light", ctx="camera section"), "image": tr("Image"), "focus": tr("Focus"),
            "framing": tr("Framing")}[section]


_TIPS = {
    "exposure_time": N_("The shutter speed: slower is brighter but blurs movement."),
    "gain": N_("The webcam's ISO: more is brighter but grainier."),
    "backlight": N_("Brightens a face in front of a bright window."),
    "anti_flicker": N_("Set it to your mains frequency to stop lights flickering or banding."),
    "frame_rate_drop": N_("Lets the webcam lower its frame rate in dim light for a brighter picture."),
}
_FLICKER = {webcam_controls.FLICKER_OFF: N_("Off"), webcam_controls.FLICKER_50: N_("50 Hz"),
            webcam_controls.FLICKER_60: N_("60 Hz"), webcam_controls.FLICKER_AUTO: N_("Auto")}


def is_webcam(source_id: Optional[str]) -> bool:
    return bool(source_id) and source_id.startswith(SOURCE_PREFIX)


def default_scanner():
    """What lists the webcams on this system: a callable returning [webcams.Device]."""
    return webcams.LinuxScanner().scan if vcam.IS_LINUX else webcams.scan_windows


def default_controls():
    """What reads and sets a webcam's picture settings: a callable taking a webcams.Device."""
    return webcam_controls.controls_for


def shutter_text(seconds: float) -> str:
    """An exposure time the way cameras show it: 1/60, or 0.5 s for long ones."""
    if seconds <= 0:
        return "–"
    if seconds >= 0.4:
        return f"{seconds:.1f} s".replace(".0 s", " s")
    return f"1/{round(1 / seconds)}"


def shown_value(control, value: int) -> str:
    """How the card shows a control's number."""
    if control.unit == "100us":
        return shutter_text(value / 10000)
    if control.unit == "log2s":
        return shutter_text(2.0 ** value)
    if control.unit == "kelvin":
        return f"{value} K"
    if control.unit == "arcsec":
        return f"{value / 3600:+.0f}°".replace("+0°", "0°")
    if control.unit == "zoom" and control.minimum > 0:
        return f"{value / control.minimum:.1f}×"
    return str(value)


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

    def __init__(self, scanner=None, controls=None):
        self._scanner = scanner  # tests pass their own; the real one is made at setup
        self._controls_for = controls

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._scan = self._scanner or default_scanner()
        self._controls_for = self._controls_for or default_controls()
        self._controls: dict = {}  # key: {name: webcam_controls.Control} as last read from the camera
        self._backends: dict = {}  # key: (webcams.Device, what reads and sets its controls)
        self._control_rows: dict = {}  # name: (row widget, set value, enable) on the card, for the camera picked
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
        for key in set(self._devices) - {d.key for d in found}:
            self._drop_backend(key)  # its handle on the camera is dead; a replugged one gets a fresh one
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
            self._controls.pop(key, None)
            self._drop_backend(key)
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
        if state == "ok":  # every opening, even one that reads like the last: a replug resets most webcams
            self._modes[key] = detail
            self._apply_controls(key)
        if self._states.get(key) == (state, detail):
            return
        self._states[key] = (state, detail)
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
            self._build_controls()
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
        self._reset_btn = card_action(tr("Reset"), "reset", tr("Put the webcam's picture settings back to its defaults"))
        self._reset_btn.clicked.connect(self.reset_controls)
        add_card_header(lay, tr("Webcam"), self.icon, hue=self.category, action=self._reset_btn)
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
        self._controls_box = QWidget()
        self._controls_lay = QVBoxLayout(self._controls_box)
        self._controls_lay.setContentsMargins(0, 0, 0, 0)
        self._controls_lay.setSpacing(lay.spacing())
        lay.addWidget(self._controls_box)

        self._fill_combos()
        self._build_controls()
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

    # ── Picture settings ──────────────────────────────────────────────────

    def controls(self, key: str) -> dict:
        """{name: Control} the webcam with this key offers, as last read."""
        return self._controls.get(key, {})

    def _saved_controls(self, key: str) -> dict:
        return self._settings.get(key, {}).get("controls", {})

    def _backend(self, key: str):
        """What reads and sets this webcam's controls, kept while it's plugged in as it is (Windows opens it once)."""
        device = self._devices.get(key)
        held = self._backends.get(key)
        if held is not None and held[0] == device:
            return held[1]
        self._drop_backend(key)
        backend = self._controls_for(device) if device is not None else None
        if backend is not None:
            self._backends[key] = (device, backend)
        return backend

    def _drop_backend(self, key: str):
        held = self._backends.pop(key, None)
        if held is not None and hasattr(held[1], "close"):
            held[1].close()

    def _read_controls(self, key: str) -> dict:
        backend = self._backend(key)
        try:
            found = {c.name: c for c in backend.list()} if backend is not None else {}
        except Exception:
            logger.exception("Couldn't read the webcam's settings")
            found = {}
        self._controls[key] = found
        return found

    def _apply_controls(self, key: str):
        """Put back what was picked for this webcam, and anti-flicker to the local mains unless something was picked."""
        backend = self._backend(key)
        if backend is None:
            return
        have = self._read_controls(key)
        wanted = dict(self._saved_controls(key))
        flicker = have.get("anti_flicker")
        if flicker is not None and "anti_flicker" not in wanted:
            local = local_flicker(flicker)
            if local is not None:
                wanted["anti_flicker"] = local
        changed = False
        for name in apply_order(wanted):
            control, auto = have.get(name), AUTO_OF.get(name)
            if control is None or (auto in have and wanted.get(auto, have[auto].value)):
                continue  # not on this camera, or its auto is on and the manual value would be refused
            value = control.clamp(wanted[name])
            if value != control.value:
                changed = backend.set(name, value) or changed
        if changed:
            self._read_controls(key)
        if key == self._selected_key:
            self._build_controls()

    def _set_control(self, name: str, value: int):
        key = self._selected_key
        backend = self._backend(key)
        if backend is None:
            return
        backend.set(name, value)
        cfg = dict(self._settings.get(key, {}))
        cfg["controls"] = {**cfg.get("controls", {}), name: int(value)}
        self._settings[key] = cfg
        self._host.schedule_save()
        if name in AUTOS:  # which manual controls do anything now
            self._read_controls(key)
            self._sync_controls()

    def reset_controls(self):
        """Every picture setting back to the camera's default (anti-flicker to the local mains)."""
        key = self._selected_key
        backend = self._backend(key)
        if backend is None:
            return
        have = self._read_controls(key)
        for name in apply_order(have):
            control = have[name]
            if AUTO_OF.get(name) in have and have[AUTO_OF[name]].default:
                continue  # auto by default: the manual value can't be set and doesn't matter
            if control.value != default_of(control):
                backend.set(name, default_of(control))
        cfg = dict(self._settings.get(key, {}))
        if cfg.pop("controls", None) is not None:
            self._settings[key] = cfg
            self._host.schedule_save()
        self._read_controls(key)
        self._sync_controls()

    def _build_controls(self):
        """The rows for what the picked webcam offers; nothing when it offers nothing."""
        if self._card is None:
            return
        while self._controls_lay.count():
            item = self._controls_lay.takeAt(0)
            if item.widget() is not None:
                item.widget().deleteLater()
        self._control_rows = {}
        key = self._selected_key
        have = self._read_controls(key) if key in self._devices else {}
        for section, icon, entries in _SECTIONS:
            shown = [(name, label) for name, label in entries if name in have]
            if not shown:
                continue
            add_section_heading(self._controls_lay, _section_title(section), icon)
            tiles = []
            for name, label in shown:
                row = self._control_row(have[name], tr(label))
                if isinstance(row, SwitchTile):
                    tiles.append(row)
                else:
                    self._controls_lay.addWidget(row)
            if tiles:
                grid = QGridLayout()
                grid.setContentsMargins(0, 0, 0, 0)
                grid.setSpacing(8)
                for i, tile in enumerate(tiles):
                    grid.addWidget(tile, i // 2, i % 2)
                grid.setColumnStretch(0, 1)
                grid.setColumnStretch(1, 1)
                holder = QWidget()
                holder.setLayout(grid)
                self._controls_lay.addWidget(holder)
        self._controls_box.setVisible(bool(self._control_rows))
        self._reset_btn.setVisible(bool(self._control_rows))
        self._sync_controls()

    def _control_row(self, control, label: str) -> QWidget:
        name = control.name
        tip = tr(_TIPS[name]) if name in _TIPS else ""
        if name in AUTOS:
            auto, manual = SegmentButton(tr("Auto"), value="Auto"), SegmentButton(tr("Manual"), value="Manual")
            group = QButtonGroup(self._controls_box)
            group.addButton(auto)
            group.addButton(manual)
            group.buttonClicked.connect(lambda button: self._set_control(name, int(button is auto)))
            row = control_row_widget(label, segmented_row(auto, manual), stretch=True)

            def show(value, auto=auto, manual=manual):
                (auto if value else manual).setChecked(True)
            self._control_rows[name] = (row, show, row.setEnabled)
            return row
        if control.kind == "bool" or (control.kind == "int" and (control.minimum, control.maximum) == (0, 1)):
            tile = SwitchTile("moon" if name == "frame_rate_drop" else "sun", label)
            tile.switch.setToolTip(tip)
            tile.switch.toggled.connect(lambda on: self._set_control(name, int(on)))

            def show(value, switch=tile.switch):
                switch.blockSignals(True)
                switch.setChecked(bool(value))
                switch.blockSignals(False)
            self._control_rows[name] = (tile, show, tile.setEnabled)
            return tile
        if control.kind == "menu":
            combo = NoScrollComboBox()
            combo.setToolTip(tip)
            for value in control.menu:
                combo.addItem(tr(_FLICKER[value]) if name == "anti_flicker" and value in _FLICKER else str(value), value)
            combo.currentIndexChanged.connect(lambda i, combo=combo: self._set_control(name, combo.itemData(i)))
            row = control_row_widget(label, combo)

            def show(value, combo=combo):
                combo.blockSignals(True)
                combo.setCurrentIndex(max(0, combo.findData(value)))
                combo.blockSignals(False)
            self._control_rows[name] = (row, show, row.setEnabled)
            return row
        slider = NoScrollSlider(Qt.Orientation.Horizontal)
        readout = value_label(shown_value(control, control.value))
        logarithmic = control.unit == "100us"
        lo, hi = control.minimum, control.maximum
        if logarithmic:
            slider.setRange(0, _LOG_STEPS)
            slider.set_default(val_to_log_pos(default_of(control), _LOG_STEPS, lo, hi))
        else:
            slider.setRange(lo, hi)
            slider.setSingleStep(control.step)
            slider.setPageStep(max(control.step, (hi - lo) // 10))
            slider.set_default(default_of(control))
        slider.setToolTip(tip)

        def value_at(pos):
            return control.clamp(log_pos_to_val(pos, _LOG_STEPS, lo, hi) if logarithmic else pos)

        def moved(pos):
            value = value_at(pos)
            readout.setText(shown_value(control, value))
            self._set_control(name, value)
        slider.valueChanged.connect(moved)
        row = control_row_widget(label, slider_row(slider, readout), stretch=True)

        def show(value):
            slider.blockSignals(True)
            slider.setValue(val_to_log_pos(value, _LOG_STEPS, lo, hi) if logarithmic else value)
            slider.blockSignals(False)
            readout.setText(shown_value(control, value))
        self._control_rows[name] = (row, show, row.setEnabled)
        return row

    def _sync_controls(self):
        """The rows show what the camera has now; a manual one is greyed out while its auto is on."""
        have = self.controls(self._selected_key)
        for name, (_row, show, enable) in self._control_rows.items():
            control = have.get(name)
            if control is None:
                continue
            show(control.value)
            auto = have.get(AUTO_OF.get(name, ""))
            enable(not (auto.value if auto is not None else control.inactive))

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
            controls = entry.get("controls")
            if isinstance(controls, dict):
                kept = {n: v for n, v in controls.items() if isinstance(n, str) and type(v) is int}
                if kept:
                    clean["controls"] = kept
            self._settings[key] = clean
        self._fill_combos()
        self._build_controls()
        self._render()

    def shutdown(self):
        self._stopped = True
        self._timer.stop()
        for key in list(self._backends):
            self._drop_backend(key)
