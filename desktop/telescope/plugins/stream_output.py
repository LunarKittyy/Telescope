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
        # Set on set_config() before phone data arrives; applied once on_phone_state() has real sizes.
        self._pending_resolution_text = None
        # Lens switch doesn't trigger fresh /v1/state fetch; use cached capabilities dict.
        bus.camera_switched.connect(self._on_camera_switched)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = QVBoxLayout(card)
        lay.setContentsMargins(16, 15, 16, 15)
        lay.setSpacing(10)
        add_card_header(lay, "Stream Output", "stream")

        # ── Resolution ────────────────────────────────────────────────────────
        add_section_heading(lay, "Output")
        self._res_combo = NoScrollComboBox()
        self._res_combo.addItem("—")
        self._res_combo.setEnabled(False)
        self._res_combo.currentIndexChanged.connect(self._on_resolution)
        lay.addLayout(_row("Resolution", self._res_combo, stretch=True))

        # ── FPS ───────────────────────────────────────────────────────────────
        self._fps_spin = NoScrollSpinBox()  # Capture and playback rate; faster just wastes power.
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
        """Return (width, height, fps) for StreamWorker (width/height always None; resolution controlled by phone)."""
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
        current = self._res_combo.currentData()  # Lens switch reuses ImageReader; resolution carries over.
        live_w, live_h = current if current else (None, None)
        self._apply_camera(cam, live_w, live_h)

    def _apply_camera(self, cam: dict, live_w, live_h):
        sizes = cam.get("supportedSizes") or []
        sizes = [(s["width"], s["height"]) for s in sizes
                 if isinstance(s, dict) and "width" in s and "height" in s]
        if not sizes:
            return

        if cam["id"] != self._current_camera_id:  # Only rebuild on actual camera change; don't fight selection.
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
            live_text = _size_label(live_w, live_h)  # Same lens; reflect live size if changed.
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
            self._pending_resolution_text = res  # Combo unpopulated at load; apply when on_phone_state() arrives.
        if fps := cfg.get("fps", cfg.get("phone_fps")):  # Fallback to legacy "phone_fps" if "fps" absent.
            self._fps_spin.setValue(int(fps))
        if q := cfg.get("jpeg_quality"):
            self._quality_slider.setValue(int(q))
