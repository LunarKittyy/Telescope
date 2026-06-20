from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QButtonGroup, QCheckBox, QFrame, QHBoxLayout, QLabel, QRadioButton,
    QVBoxLayout, QWidget,
)

from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    LogSliderRow, NoScrollSlider, WbSliderRow, create_separator,
    create_vector_icon, ns_to_display,
)
from telescope.widgets.lens_panel import LensPanel

_FOCUS_STEPS = 1000


def _diopters_to_label(d: float) -> str:
    if d <= 0.01:
        return "inf"
    return f"{1.0 / d:.2f} m"


def _row(label: str, widget, label_width=110, stretch=False) -> QHBoxLayout:
    from PyQt6.QtWidgets import QLayout
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    lbl = QLabel(label)
    lbl.setObjectName("dim")
    lbl.setFixedWidth(label_width)
    lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
    lay.addWidget(lbl)
    if stretch:
        if isinstance(widget, QLayout):
            lay.addLayout(widget, 1)
        else:
            lay.addWidget(widget, 1)
    else:
        if isinstance(widget, QLayout):
            lay.addLayout(widget)
        else:
            lay.addWidget(widget)
        lay.addStretch(1)
    return lay


class CameraControlPlugin(TelescopePlugin):
    name = "camera_control"

    def setup(self, host, bus):
        self._host              = host
        self._ctrl              = None
        self._manual_exp        = False
        self._manual_wb         = False
        self._manual_focus      = False
        self._focus_max_diopters: float = 10.0
        bus.phone_state_updated.connect(self._on_phone_state_from_bus)

    def create_panel(self) -> QWidget:
        card = QFrame()
        card.setFrameShape(QFrame.Shape.StyledPanel)
        card.setObjectName("card")
        lay = QVBoxLayout(card)
        lay.setContentsMargins(14, 14, 14, 14)
        lay.setSpacing(10)

        hdr = QHBoxLayout()
        hdr.setContentsMargins(0, 0, 0, 4)
        hdr.setSpacing(8)
        icon_lbl = QLabel()
        icon_lbl.setPixmap(create_vector_icon("camera", "#518cc6").pixmap(18, 18))
        icon_lbl.setFixedSize(18, 18)
        hdr.addWidget(icon_lbl)
        title_lbl = QLabel("Camera")
        title_lbl.setObjectName("card_title")
        hdr.addWidget(title_lbl)
        hdr.addStretch()
        lay.addLayout(hdr)

        # ── Lens ──────────────────────────────────────────────────────────────
        lens_row = QHBoxLayout()
        lens_row.setContentsMargins(0, 0, 0, 0)
        ll = QLabel("Lens")
        ll.setObjectName("dim")
        ll.setFixedWidth(110)
        ll.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        lens_row.addWidget(ll)
        self._lens_panel = LensPanel()
        self._lens_panel.lens_selected.connect(self._on_lens_selected)
        lens_row.addWidget(self._lens_panel, 1)
        lay.addLayout(lens_row)

        self._cam_info_lbl = QLabel("")
        self._cam_info_lbl.setObjectName("dim")
        self._cam_info_lbl.setWordWrap(True)
        lay.addLayout(_row("", self._cam_info_lbl, stretch=True))

        lay.addWidget(create_separator())

        # ── Exposure ──────────────────────────────────────────────────────────
        exp_row = QHBoxLayout()
        exp_row.setContentsMargins(0, 0, 0, 0)
        el = QLabel("Exposure")
        el.setObjectName("dim")
        el.setFixedWidth(110)
        el.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        exp_row.addWidget(el)
        self._rb_exp_auto   = QRadioButton("Auto")
        self._rb_exp_manual = QRadioButton("Manual")
        for rb in (self._rb_exp_auto, self._rb_exp_manual):
            rb.setAutoExclusive(False)
        self._exp_grp = QButtonGroup(card)
        self._exp_grp.addButton(self._rb_exp_auto)
        self._exp_grp.addButton(self._rb_exp_manual)
        self._rb_exp_auto.setChecked(True)
        self._exp_grp.buttonClicked.connect(lambda _: self._on_exp_mode())
        exp_row.addWidget(self._rb_exp_auto)
        exp_row.addWidget(self._rb_exp_manual)
        exp_row.addStretch()
        lay.addLayout(exp_row)

        self._iso_slider = LogSliderRow(
            v_min=50, v_max=6400,
            display_fn=lambda v: f"ISO {int(round(v))}",
        )
        self._iso_slider.value_changed.connect(self._on_iso_changed)
        lay.addLayout(_row("ISO", self._iso_slider, stretch=True))
        self._iso_slider.set_enabled(False)

        self._sht_slider = LogSliderRow(
            v_min=100_000, v_max=1_000_000_000,
            display_fn=lambda v: ns_to_display(int(round(v))),
            spinbox_suffix=" ms",
            spinbox_scale=1e-6,
            spinbox_decimals=2,
        )
        self._sht_slider.value_changed.connect(self._on_shutter_changed)
        lay.addLayout(_row("Shutter", self._sht_slider, stretch=True))
        self._sht_slider.set_enabled(False)

        lay.addWidget(create_separator())

        # ── White Balance ─────────────────────────────────────────────────────
        wb_row = QHBoxLayout()
        wb_row.setContentsMargins(0, 0, 0, 0)
        wl = QLabel("White bal.")
        wl.setObjectName("dim")
        wl.setFixedWidth(110)
        wl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        wb_row.addWidget(wl)
        self._rb_wb_auto   = QRadioButton("Auto")
        self._rb_wb_manual = QRadioButton("Manual")
        for rb in (self._rb_wb_auto, self._rb_wb_manual):
            rb.setAutoExclusive(False)
        self._wb_grp = QButtonGroup(card)
        self._wb_grp.addButton(self._rb_wb_auto)
        self._wb_grp.addButton(self._rb_wb_manual)
        self._rb_wb_auto.setChecked(True)
        self._wb_grp.buttonClicked.connect(lambda _: self._on_wb_mode())
        wb_row.addWidget(self._rb_wb_auto)
        wb_row.addWidget(self._rb_wb_manual)
        wb_row.addStretch()
        lay.addLayout(wb_row)

        self._wb_slider = WbSliderRow()
        self._wb_slider.value_changed.connect(self._on_wb_changed)
        lay.addLayout(_row("Temperature", self._wb_slider, stretch=True))
        self._wb_slider.set_enabled(False)

        lay.addWidget(create_separator())

        # ── OIS ───────────────────────────────────────────────────────────────
        ois_row = QHBoxLayout()
        ois_row.setContentsMargins(0, 0, 0, 0)
        ol = QLabel("OIS")
        ol.setObjectName("dim")
        ol.setFixedWidth(110)
        ol.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        ois_row.addWidget(ol)
        self._ois_cb = QCheckBox("Optical Image Stabilization")
        self._ois_cb.setChecked(True)
        self._ois_cb.toggled.connect(self._on_ois)
        ois_row.addWidget(self._ois_cb)
        ois_row.addStretch()
        lay.addLayout(ois_row)

        lay.addWidget(create_separator())

        # ── Focus ─────────────────────────────────────────────────────────────
        focus_row = QHBoxLayout()
        focus_row.setContentsMargins(0, 0, 0, 0)
        fl = QLabel("Focus")
        fl.setObjectName("dim")
        fl.setFixedWidth(110)
        fl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        focus_row.addWidget(fl)
        self._rb_focus_auto   = QRadioButton("Auto")
        self._rb_focus_manual = QRadioButton("Manual")
        for rb in (self._rb_focus_auto, self._rb_focus_manual):
            rb.setAutoExclusive(False)
        self._focus_grp = QButtonGroup(card)
        self._focus_grp.addButton(self._rb_focus_auto)
        self._focus_grp.addButton(self._rb_focus_manual)
        self._rb_focus_auto.setChecked(True)
        self._focus_grp.buttonClicked.connect(lambda _: self._on_focus_mode())
        focus_row.addWidget(self._rb_focus_auto)
        focus_row.addWidget(self._rb_focus_manual)
        focus_row.addStretch()
        lay.addLayout(focus_row)

        focus_slider_row = QHBoxLayout()
        focus_slider_row.setContentsMargins(0, 0, 0, 0)
        focus_slider_row.setSpacing(8)
        self._focus_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._focus_slider.setRange(0, _FOCUS_STEPS)
        self._focus_slider.setValue(0)
        self._focus_slider.setEnabled(False)
        self._focus_slider.valueChanged.connect(self._on_focus_slider)
        self._focus_val_lbl = QLabel("inf")
        self._focus_val_lbl.setObjectName("dim")
        self._focus_val_lbl.setFixedWidth(60)
        focus_slider_row.addWidget(self._focus_slider, 1)
        focus_slider_row.addWidget(self._focus_val_lbl)
        lay.addLayout(_row("Distance", focus_slider_row, stretch=True))

        return card

    # ── Stream lifecycle ──────────────────────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        self._lens_panel.set_placeholder("Loading lenses...")

    def on_stream_stop(self):
        self._ctrl = None
        self._lens_panel.clear()
        self._cam_info_lbl.setText("")

    def on_phone_state(self, state: dict):
        if not state:
            self._lens_panel.set_placeholder("Unavailable")
            return
        cameras      = state.get("cameras", [])
        is_auto      = state.get("auto", True)
        wb_kelvin    = state.get("wb_kelvin")
        ois          = state.get("ois", True)
        iso_val      = state.get("iso")
        sht_val      = state.get("shutter_ns")
        focus_mode   = state.get("focus_mode", "continuous")
        focus_dist   = state.get("focus_distance", 0.0)

        self._lens_panel.load(cameras)

        cur = next((c for c in cameras if c.get("current")), None)
        if cur:
            self._iso_slider.set_range(cur.get("isoMin", 50), cur.get("isoMax", 6400))
            self._sht_slider.set_range(
                cur.get("shutterMinNs", 100_000),
                cur.get("shutterMaxNs", 1_000_000_000),
            )
            self._update_cam_info_lbl(cur)
            self._update_camera_caps(
                cur.get("supportsManualSensor", True),
                cur.get("supportsManualWB", True),
                cur.get("supportsManualFocus", False),
                float(cur.get("minFocusDistance", 10.0)),
            )

        self._rb_exp_auto.setChecked(is_auto)
        self._rb_exp_manual.setChecked(not is_auto)
        self._manual_exp = not is_auto
        self._iso_slider.set_enabled(not is_auto)
        self._sht_slider.set_enabled(not is_auto)
        if iso_val: self._iso_slider.set_value(float(iso_val))
        if sht_val: self._sht_slider.set_value(float(sht_val))

        manual_wb = wb_kelvin is not None
        self._rb_wb_auto.setChecked(not manual_wb)
        self._rb_wb_manual.setChecked(manual_wb)
        self._manual_wb = manual_wb
        self._wb_slider.set_enabled(manual_wb)
        if wb_kelvin: self._wb_slider.set_value(int(wb_kelvin))

        self._ois_cb.setChecked(bool(ois))

        manual_focus = focus_mode == "manual"
        self._rb_focus_auto.setChecked(not manual_focus)
        self._rb_focus_manual.setChecked(manual_focus)
        self._manual_focus = manual_focus
        self._focus_slider.setEnabled(manual_focus)
        self._set_focus_slider_value(float(focus_dist))

    def _on_phone_state_from_bus(self, state: dict):
        # Called via bus signal — only update if we're mid-stream and this is
        # a monitoring poll result (has battery key), not a camera fetch.
        # Camera fetches are routed through app.py → on_phone_state directly.
        pass

    # ── Camera capability gating ──────────────────────────────────────────────

    def _update_cam_info_lbl(self, cam: dict):
        hw    = cam.get("hwLevel", "")
        parts = []
        if hw:
            parts.append(hw)
        parts.append("manual sensor " + ("✓" if cam.get("supportsManualSensor") else "✗"))
        parts.append("manual WB "     + ("✓" if cam.get("supportsManualWB")     else "✗"))
        parts.append("manual focus "  + ("✓" if cam.get("supportsManualFocus")  else "✗"))
        parts.append("OIS "           + ("✓" if cam.get("hasOis")               else "✗"))
        self._cam_info_lbl.setText("  ·  ".join(parts))

    def _update_camera_caps(self, supports_manual_sensor: bool, supports_manual_wb: bool,
                            supports_manual_focus: bool = False, min_focus_distance: float = 10.0):
        self._rb_exp_manual.setEnabled(supports_manual_sensor)
        if not supports_manual_sensor:
            self._rb_exp_auto.setChecked(True)
            self._rb_exp_manual.setChecked(False)
            self._manual_exp = False
            self._iso_slider.set_enabled(False)
            self._sht_slider.set_enabled(False)
            self._rb_exp_manual.setToolTip("This camera does not support MANUAL_SENSOR")
        else:
            self._rb_exp_manual.setToolTip("")

        self._rb_wb_manual.setEnabled(supports_manual_wb)
        if not supports_manual_wb:
            self._rb_wb_auto.setChecked(True)
            self._rb_wb_manual.setChecked(False)
            self._manual_wb = False
            self._wb_slider.set_enabled(False)
            self._rb_wb_manual.setToolTip("This camera does not support MANUAL_POST_PROCESSING")
        else:
            self._rb_wb_manual.setToolTip("")

        self._focus_max_diopters = min_focus_distance if min_focus_distance > 0 else 10.0
        self._rb_focus_manual.setEnabled(supports_manual_focus)
        if not supports_manual_focus:
            self._rb_focus_auto.setChecked(True)
            self._rb_focus_manual.setChecked(False)
            self._manual_focus = False
            self._focus_slider.setEnabled(False)
            self._rb_focus_manual.setToolTip("This camera does not support manual focus")
        else:
            self._rb_focus_manual.setToolTip("")

    # ── Handlers ──────────────────────────────────────────────────────────────

    def _on_lens_selected(self, cam: dict):
        if self._ctrl:
            self._ctrl.send(action="camera", id=cam["id"])
            self._iso_slider.set_range(cam.get("isoMin", 50), cam.get("isoMax", 6400))
            self._sht_slider.set_range(
                cam.get("shutterMinNs", 100_000),
                cam.get("shutterMaxNs", 1_000_000_000),
            )
            self._update_cam_info_lbl(cam)
            self._update_camera_caps(
                cam.get("supportsManualSensor", True),
                cam.get("supportsManualWB", True),
                cam.get("supportsManualFocus", False),
                float(cam.get("minFocusDistance", 10.0)),
            )

    def _on_exp_mode(self):
        manual = self._rb_exp_manual.isChecked()
        self._manual_exp = manual
        self._iso_slider.set_enabled(manual)
        self._sht_slider.set_enabled(manual)
        if self._ctrl:
            if not manual:
                self._ctrl.send(action="auto")
            else:
                self._ctrl.send(action="iso",     value=int(self._iso_slider.get_value()))
                self._ctrl.send(action="shutter", value=int(self._sht_slider.get_value()))
        self._host._schedule_save()

    def _on_iso_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="iso", value=int(val))
        self._host._schedule_save()

    def _on_shutter_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="shutter", value=int(val))
        self._host._schedule_save()

    def _on_wb_mode(self):
        manual = self._rb_wb_manual.isChecked()
        self._manual_wb = manual
        self._wb_slider.set_enabled(manual)
        if self._ctrl:
            if not manual:
                self._ctrl.send(action="wb_auto")
            else:
                self._ctrl.send(action="wb_kelvin", value=self._wb_slider.get_value())

    def _on_wb_changed(self, k: int):
        if self._ctrl and self._manual_wb:
            self._ctrl.send(action="wb_kelvin", value=k)

    def _on_ois(self, checked: bool):
        if self._ctrl:
            self._ctrl.send(action="ois", value="1" if checked else "0")
        self._host._schedule_save()

    def _on_focus_mode(self):
        manual = self._rb_focus_manual.isChecked()
        self._manual_focus = manual
        self._focus_slider.setEnabled(manual)
        if self._ctrl:
            if not manual:
                self._ctrl.send(action="focus_mode", value="continuous")
            else:
                self._ctrl.send(action="focus_mode", value="manual")
                self._ctrl.send(action="focus_distance",
                                value=self._slider_to_diopters(self._focus_slider.value()))
        self._host._schedule_save()

    def _on_focus_slider(self, pos: int):
        d = self._slider_to_diopters(pos)
        self._focus_val_lbl.setText(_diopters_to_label(d))
        if self._ctrl and self._manual_focus:
            self._ctrl.send(action="focus_distance", value=d)
        self._host._schedule_save()

    def _slider_to_diopters(self, pos: int) -> float:
        return (pos / _FOCUS_STEPS) * self._focus_max_diopters

    def _set_focus_slider_value(self, diopters: float):
        pos = int((diopters / self._focus_max_diopters) * _FOCUS_STEPS) \
              if self._focus_max_diopters > 0 else 0
        self._focus_slider.blockSignals(True)
        self._focus_slider.setValue(max(0, min(_FOCUS_STEPS, pos)))
        self._focus_slider.blockSignals(False)
        self._focus_val_lbl.setText(_diopters_to_label(diopters))

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {
            "exp_manual":      self._rb_exp_manual.isChecked(),
            "iso":             self._iso_slider.get_value(),
            "shutter_ns":      self._sht_slider.get_value(),
            "ois":             self._ois_cb.isChecked(),
            "focus_manual":    self._rb_focus_manual.isChecked(),
            "focus_diopters":  self._slider_to_diopters(self._focus_slider.value()),
        }

    def set_config(self, cfg: dict):
        if cfg.get("exp_manual"):
            self._rb_exp_manual.setChecked(True)
            self._rb_exp_auto.setChecked(False)
            self._iso_slider.set_enabled(True)
            self._sht_slider.set_enabled(True)
        if iso := cfg.get("iso"):
            self._iso_slider.set_value(float(iso))
        if sht := cfg.get("shutter_ns"):
            self._sht_slider.set_value(float(sht))
        self._ois_cb.setChecked(cfg.get("ois", True))
        if cfg.get("focus_manual"):
            self._rb_focus_manual.setChecked(True)
            self._rb_focus_auto.setChecked(False)
        if d := cfg.get("focus_diopters"):
            self._set_focus_slider_value(float(d))
