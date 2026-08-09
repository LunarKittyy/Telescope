from PyQt6.QtCore import Qt, QTimer
from PyQt6.QtWidgets import (
    QFrame, QHBoxLayout, QLabel, QVBoxLayout, QWidget,
)

from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, NoScrollSpinBox, add_card_header,
    add_section_heading, control_row as _row, create_card, create_separator,
    quality_label, stretch_slider,
)

_DEFAULT_QUALITY = 85
_DEFAULT_FPS     = 30


def _size_label(w: int, h: int) -> str:
    return f"{w} x {h}"


class StreamOutputPlugin(TelescopePlugin):
    name = "stream_output"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._ctrl = None
        self._current_camera_id = None
        # Set on set_config() before any phone data has arrived; applied the
        # first time on_phone_state() has real sizes to match it against.
        self._pending_resolution_text = None
        # A lens switch (camera_control.py) doesn't trigger a fresh /v1/state
        # fetch - it hands over the capability dict it already has cached, so
        # this combo can be kept in sync with the new lens's supported sizes
        # the same way camera_control.py refreshes its own ISO/shutter ranges.
        bus.camera_switched.connect(self._on_camera_switched)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = QVBoxLayout(card)
        lay.setContentsMargins(16, 15, 16, 15)
        lay.setSpacing(10)
        add_card_header(lay, "Stream Output", "stream")

        # ── Resolution ────────────────────────────────────────────────────────
        # Populated from the phone's actual capture sizes for the current lens
        # (on_phone_state), not a fixed list - unlike the old post-decode
        # resize this used to drive, a selection here now changes what the
        # phone captures at, so the option set has to match what that specific
        # camera can actually do.
        add_section_heading(lay, "Output")
        self._res_combo = NoScrollComboBox()
        self._res_combo.addItem("—")
        self._res_combo.setEnabled(False)
        self._res_combo.currentIndexChanged.connect(self._on_resolution)
        lay.addLayout(_row("Resolution", self._res_combo, stretch=True))

        # ── FPS ───────────────────────────────────────────────────────────────
        # One control, not separate "playback" and "phone" fps sliders - the
        # desktop's virtual camera can't show motion the phone never captured,
        # and capturing faster than it's played back just burns phone battery
        # and Wi-Fi bandwidth for frames nothing ever displays. This drives
        # both the phone's capture rate and the local vcam's pacing together.
        self._fps_spin = NoScrollSpinBox()
        self._fps_spin.setRange(5, 60)
        self._fps_spin.setValue(_DEFAULT_FPS)
        self._fps_spin.setSuffix(" fps")
        self._fps_spin.setFixedWidth(90)
        self._fps_spin.setToolTip("Both the phone's capture rate and the local virtual camera's "
                                   "playback rate. Lower reduces bandwidth and phone battery use.")
        self._fps_spin.editingFinished.connect(self._on_fps)
        lay.addLayout(_row("FPS", self._fps_spin))

        lay.addWidget(create_separator())

        # ── JPEG Quality ──────────────────────────────────────────────────────
        add_section_heading(lay, "Phone stream")
        self._quality_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._quality_slider.setRange(1, 100)
        self._quality_slider.setValue(_DEFAULT_QUALITY)
        stretch_slider(self._quality_slider, 104)
        self._quality_slider.setToolTip(
            "Lower quality and FPS reduce bandwidth. Useful on slow Wi-Fi or USB 2. "
            "Very low values are a last resort - the image gets blocky fast."
        )
        self._quality_val_lbl = QLabel(quality_label(_DEFAULT_QUALITY))
        self._quality_val_lbl.setObjectName("val")
        self._quality_val_lbl.setMinimumWidth(92)
        self._quality_slider.valueChanged.connect(self._on_quality_changed)
        q_inner = QHBoxLayout()
        q_inner.setContentsMargins(0, 0, 0, 0)
        q_inner.setSpacing(8)
        q_inner.addWidget(self._quality_slider, 1)
        q_inner.addWidget(self._quality_val_lbl)
        lay.addLayout(_row("JPEG quality", q_inner, stretch=True))

        return card

    def get_stream_params(self) -> tuple:
        """Return (width, height, fps) for StreamWorker construction.

        Width/height are always pass-through (None) now - the Resolution
        control below changes what the phone captures at, so there's nothing
        left for the desktop side to resize post-decode."""
        return None, None, self._fps_spin.value()

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        QTimer.singleShot(1500, self._push_initial_settings)

    def on_stream_stop(self):
        self._ctrl = None
        self._current_camera_id = None
        self._res_combo.blockSignals(True)
        self._res_combo.clear()
        self._res_combo.addItem("—")
        self._res_combo.setEnabled(False)
        self._res_combo.blockSignals(False)

    def _push_initial_settings(self):
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=self._quality_slider.value())
            self._ctrl.send(action="fps_target",   value=self._fps_spin.value())

    def on_phone_state(self, state: dict):
        cams = state.get("cameras")
        if not isinstance(cams, list):
            return
        cur = next((c for c in cams if c.get("current")), None)
        if cur is None:
            return
        self._apply_camera(cur, state.get("stream_width"), state.get("stream_height"))

    def _on_camera_switched(self, cam: dict):
        # A plain lens switch (CameraSessionController.switchCameraTo) reuses
        # the existing ImageReader unchanged - it never touches
        # streamWidth/streamHeight, so the live resolution after the switch
        # is just whatever it already was, not the new lens's largest size.
        # No fresh /v1/state comes back on a lens switch to confirm that, so
        # the desktop's own current selection (if the new lens still
        # supports it) is the correct live value to carry over - not a
        # guess, since that's genuinely what the phone does.
        current = self._res_combo.currentData()
        live_w, live_h = current if current else (None, None)
        self._apply_camera(cam, live_w, live_h)

    def _apply_camera(self, cam: dict, live_w, live_h):
        sizes = cam.get("supportedSizes") or []
        sizes = [(s["width"], s["height"]) for s in sizes
                 if isinstance(s, dict) and "width" in s and "height" in s]
        if not sizes:
            return

        # Only rebuild the item list when the lens actually changed - doing
        # it on every state refresh would otherwise fight the user's
        # in-progress selection.
        if cam["id"] != self._current_camera_id:
            self._current_camera_id = cam["id"]
            self._res_combo.blockSignals(True)
            self._res_combo.clear()
            for w, h in sizes:
                self._res_combo.addItem(_size_label(w, h), (w, h))
            target_text = self._pending_resolution_text
            idx = self._res_combo.findText(target_text) if target_text else -1
            if idx < 0 and live_w and live_h:
                idx = self._res_combo.findText(_size_label(live_w, live_h))
            self._res_combo.setCurrentIndex(idx if idx >= 0 else 0)
            self._res_combo.setEnabled(True)
            self._res_combo.blockSignals(False)
            self._pending_resolution_text = None
        elif live_w and live_h:
            # Same lens, but the phone's live size doesn't match our
            # selection (e.g. a reconnect restored an older setting) -
            # reflect reality without re-sending a control we didn't ask for.
            live_text = _size_label(live_w, live_h)
            if self._res_combo.currentText() != live_text:
                idx = self._res_combo.findText(live_text)
                if idx >= 0:
                    self._res_combo.blockSignals(True)
                    self._res_combo.setCurrentIndex(idx)
                    self._res_combo.blockSignals(False)

    # ── Handlers ──────────────────────────────────────────────────────────────

    def _on_resolution(self):
        size = self._res_combo.currentData()
        if size is None or self._ctrl is None:
            return
        w, h = size
        self._ctrl.send(action="resolution", width=w, height=h)
        self._bus.resolution_change_requested.emit(w, h)
        self._host.schedule_save()

    def _on_fps(self):
        fps = self._fps_spin.value()
        self._host.update_stream_output(fps=fps)
        if self._ctrl:
            self._ctrl.send(action="fps_target", value=fps)
        self._host.schedule_save()

    def _on_quality_changed(self, q: int):
        self._quality_val_lbl.setText(quality_label(q))
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=q)
        self._host.schedule_save()

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        cfg = {
            "fps":          self._fps_spin.value(),
            "jpeg_quality": self._quality_slider.value(),
        }
        if self._res_combo.currentData() is not None:
            cfg["resolution"] = self._res_combo.currentText()
        return cfg

    def set_config(self, cfg: dict):
        if res := cfg.get("resolution"):
            # The combo isn't populated yet at load time (no phone data),
            # so this is applied once on_phone_state() has real sizes.
            self._pending_resolution_text = res
        # "phone_fps" is read as a fallback for a config saved before
        # playback/phone fps were merged into one control - "fps" (the old
        # playback-only value) wins if both are present, matching what the
        # combined spinner would already have shown for most users, since the
        # two were rarely set to different values on purpose.
        if fps := cfg.get("fps", cfg.get("phone_fps")):
            self._fps_spin.setValue(int(fps))
        if q := cfg.get("jpeg_quality"):
            self._quality_slider.setValue(int(q))
