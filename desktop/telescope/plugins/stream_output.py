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

RESOLUTIONS = {
    "Pass-through (auto)": None,
    "1920 x 1080": (1920, 1080),
    "1280 x 720":  (1280,  720),
    "854 x 480":   ( 854,  480),
    "640 x 360":   ( 640,  360),
}

_DEFAULT_QUALITY   = 85
_DEFAULT_PHONE_FPS = 30


class StreamOutputPlugin(TelescopePlugin):
    name = "stream_output"

    def setup(self, host, bus):
        self._host = host
        self._ctrl = None

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = QVBoxLayout(card)
        lay.setContentsMargins(16, 15, 16, 15)
        lay.setSpacing(10)
        add_card_header(lay, "Stream Output", "stream")

        # ── Resolution ────────────────────────────────────────────────────────
        add_section_heading(lay, "Output")
        self._res_combo = NoScrollComboBox()
        self._res_combo.addItems(list(RESOLUTIONS.keys()))
        self._res_combo.currentTextChanged.connect(self._on_resolution)
        lay.addLayout(_row("Resolution", self._res_combo, stretch=True))

        # ── Playback FPS ──────────────────────────────────────────────────────
        self._fps_spin = NoScrollSpinBox()
        self._fps_spin.setRange(1, 120)
        self._fps_spin.setValue(30)
        self._fps_spin.setSuffix(" fps")
        self._fps_spin.setFixedWidth(90)
        self._fps_spin.editingFinished.connect(self._on_fps)
        lay.addLayout(_row("Playback FPS", self._fps_spin))

        lay.addWidget(create_separator())

        # ── JPEG Quality ──────────────────────────────────────────────────────
        add_section_heading(lay, "Phone stream")
        self._quality_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._quality_slider.setRange(50, 100)
        self._quality_slider.setValue(_DEFAULT_QUALITY)
        stretch_slider(self._quality_slider, 104)
        self._quality_slider.setToolTip("Lower quality and FPS reduce bandwidth. Useful on slow Wi-Fi or USB 2.")
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

        # ── Phone FPS ─────────────────────────────────────────────────────────
        self._phone_fps_spin = NoScrollSpinBox()
        self._phone_fps_spin.setRange(5, 60)
        self._phone_fps_spin.setValue(_DEFAULT_PHONE_FPS)
        self._phone_fps_spin.setSuffix(" fps")
        self._phone_fps_spin.setFixedWidth(90)
        self._phone_fps_spin.setToolTip("Lower quality and FPS reduce bandwidth. Useful on slow Wi-Fi or USB 2.")
        self._phone_fps_spin.editingFinished.connect(self._on_phone_fps_changed)
        lay.addLayout(_row("Phone FPS", self._phone_fps_spin))

        return card

    def get_stream_params(self) -> tuple:
        """Return (width, height, fps) for StreamWorker construction."""
        res = RESOLUTIONS.get(self._res_combo.currentText())
        w, h = res if res else (None, None)
        return w, h, self._fps_spin.value()

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        QTimer.singleShot(1500, self._push_initial_settings)

    def on_stream_stop(self):
        self._ctrl = None

    def _push_initial_settings(self):
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=self._quality_slider.value())
            self._ctrl.send(action="fps_target",   value=self._phone_fps_spin.value())

    # ── Handlers ──────────────────────────────────────────────────────────────

    def _on_resolution(self):
        res = RESOLUTIONS.get(self._res_combo.currentText())
        w, h = res if res else (None, None)
        # None width/height is meaningful here (pass-through / no resize);
        # update_stream_output is a no-op when nothing is streaming.
        self._host.update_stream_output(width=w, height=h)
        self._host.schedule_save()

    def _on_fps(self):
        self._host.update_stream_output(fps=self._fps_spin.value())
        self._host.schedule_save()

    def _on_quality_changed(self, q: int):
        self._quality_val_lbl.setText(quality_label(q))
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=q)
        self._host.schedule_save()

    def _on_phone_fps_changed(self):
        fps = self._phone_fps_spin.value()
        if self._ctrl:
            self._ctrl.send(action="fps_target", value=fps)
        self._host.schedule_save()

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {
            "resolution":   self._res_combo.currentText(),
            "fps":          self._fps_spin.value(),
            "jpeg_quality": self._quality_slider.value(),
            "phone_fps":    self._phone_fps_spin.value(),
        }

    def set_config(self, cfg: dict):
        if res := cfg.get("resolution"):
            idx = self._res_combo.findText(res)
            if idx >= 0:
                self._res_combo.setCurrentIndex(idx)
        if fps := cfg.get("fps"):
            self._fps_spin.setValue(int(fps))
        if q := cfg.get("jpeg_quality"):
            self._quality_slider.setValue(int(q))
        if pfps := cfg.get("phone_fps"):
            self._phone_fps_spin.setValue(int(pfps))
