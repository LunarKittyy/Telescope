import math
from dataclasses import dataclass
from typing import Optional

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QButtonGroup, QCheckBox, QFrame, QHBoxLayout, QLabel, QPushButton,
    QRadioButton, QVBoxLayout, QWidget,
)

from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    LogSliderRow, NoScrollComboBox, NoScrollSlider, create_separator,
    create_vector_icon, ns_to_display,
)
from telescope.widgets.lens_panel import LensPanel

_FOCUS_STEPS = 1000
_NR_MODES    = [("Off", 0), ("Fast", 1), ("High Quality", 2)]
_EDGE_MODES  = [("Off", 0), ("Fast", 1), ("High Quality", 2)]
_WB_MIN_K    = 2000
_WB_MAX_K    = 10000
_WB_NEUTRAL  = 5500   # neutral point where R gain == B gain


def _kelvin_to_rggb(kelvin: int, tint: float) -> tuple[float, float, float, float]:
    """Convert Kelvin + tint to Camera2 RGGB channel gains.

    Uses an exponential model centred at neutral (~5500K) so warm/cool shifts
    are symmetric and the full slider range produces visually dramatic results.
    tint: -150 (green) to +150 (magenta).
    """
    t = max(-1.0, min(1.0, (kelvin - _WB_NEUTRAL) / float(_WB_NEUTRAL)))
    r = max(0.3, 2.0 * (2.0 **  t))   # high at high K = warm
    b = max(0.3, 2.0 * (2.0 ** -t))   # high at low K = cool
    g = max(0.5, min(2.5, 1.0 - tint / 500.0))
    return r, g, g, b


def _diopters_to_label(d: float) -> str:
    if d <= 0.01:
        return "inf"
    return f"{1.0 / d:.2f} m"


@dataclass(frozen=True)
class CameraControlView:
    """Everything on_phone_state needs to update the widgets, computed once
    from a raw phone-state dict and kept separate from the Qt calls that
    apply it - the state-to-values mapping is a pure function, independently
    testable without a QApplication."""

    lenses: list
    current_camera: Optional[dict]
    manual_exposure: bool
    iso: Optional[float]
    shutter_ns: Optional[float]
    manual_wb: bool
    ois: bool
    manual_focus: bool
    focus_distance: float
    ae_comp: int
    ae_comp_step: float
    ae_comp_range: tuple
    nr_mode_index: int
    edge_mode_index: int
    black_level_lock: bool
    torch: bool


def derive_camera_control_view(state: dict) -> Optional[CameraControlView]:
    """Pure state -> view mapping. Returns None for an empty/unavailable
    state (the caller should show the "Unavailable" placeholder instead)."""
    if not state:
        return None

    cameras = state.get("cameras", [])
    cur = next((c for c in cameras if c.get("current")), None)
    is_auto = state.get("auto", True)

    ae_min = cur.get("aeCompMin", -8) if cur else -8
    ae_max = cur.get("aeCompMax", 8) if cur else 8
    ae_step = float(cur.get("aeCompStep", 0.167)) if cur else 0.167

    nr_mode = state.get("nr_mode", 1)
    nr_idx = next((i for i, (_, v) in enumerate(_NR_MODES) if v == nr_mode), 1)
    edge_mode = state.get("edge_mode", 1)
    edge_idx = next((i for i, (_, v) in enumerate(_EDGE_MODES) if v == edge_mode), 1)

    return CameraControlView(
        lenses=cameras,
        current_camera=cur,
        manual_exposure=not is_auto,
        iso=state.get("iso"),
        shutter_ns=state.get("shutter_ns"),
        manual_wb=state.get("wb_manual", False),
        ois=bool(state.get("ois", True)),
        manual_focus=state.get("focus_mode", "continuous") == "manual",
        focus_distance=float(state.get("focus_distance", 0.0)),
        ae_comp=int(state.get("ae_comp", 0)),
        ae_comp_step=ae_step,
        ae_comp_range=(ae_min, ae_max),
        nr_mode_index=nr_idx,
        edge_mode_index=edge_idx,
        black_level_lock=bool(state.get("black_level_lock", False)),
        torch=bool(state.get("torch", False)),
    )


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
        self._ae_comp_step: float = 0.167
        self._torch_on: bool = False
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

        self._ae_comp_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._ae_comp_slider.setRange(-8, 8)
        self._ae_comp_slider.setValue(0)
        self._ae_comp_lbl = QLabel("0.0 EV")
        self._ae_comp_lbl.setObjectName("val")
        self._ae_comp_lbl.setFixedWidth(52)
        ae_inner = QHBoxLayout()
        ae_inner.setContentsMargins(0, 0, 0, 0)
        ae_inner.setSpacing(8)
        ae_inner.addWidget(self._ae_comp_slider, 1)
        ae_inner.addWidget(self._ae_comp_lbl)
        self._ae_comp_slider.valueChanged.connect(self._on_ae_comp_changed)
        lay.addLayout(_row("EV Comp.", ae_inner, stretch=True))

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

        self._wb_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._wb_slider.setRange(_WB_MIN_K, _WB_MAX_K)
        self._wb_slider.setValue(_WB_NEUTRAL)
        self._wb_slider.setSingleStep(100)
        self._wb_k_lbl = QLabel(f"{_WB_NEUTRAL} K")
        self._wb_k_lbl.setObjectName("val")
        self._wb_k_lbl.setFixedWidth(64)
        wb_inner = QHBoxLayout()
        wb_inner.setContentsMargins(0, 0, 0, 0)
        wb_inner.setSpacing(8)
        wb_inner.addWidget(self._wb_slider, 1)
        wb_inner.addWidget(self._wb_k_lbl)
        self._wb_slider.valueChanged.connect(self._on_wb_changed)
        lay.addLayout(_row("Temperature", wb_inner, stretch=True))
        self._wb_slider.setEnabled(False)
        self._wb_k_lbl.setEnabled(False)

        self._tint_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._tint_slider.setRange(-150, 150)
        self._tint_slider.setValue(0)
        self._tint_lbl = QLabel("0")
        self._tint_lbl.setObjectName("val")
        self._tint_lbl.setFixedWidth(28)
        tint_inner = QHBoxLayout()
        tint_inner.setContentsMargins(0, 0, 0, 0)
        tint_inner.setSpacing(8)
        tint_inner.addWidget(self._tint_slider, 1)
        tint_g = QLabel("G")
        tint_g.setObjectName("dim")
        tint_m = QLabel("M")
        tint_m.setObjectName("dim")
        tint_inner.insertWidget(0, tint_g)
        tint_inner.addWidget(tint_m)
        tint_inner.addWidget(self._tint_lbl)
        self._tint_slider.valueChanged.connect(self._on_tint_changed)
        lay.addLayout(_row("Tint", tint_inner, stretch=True))
        self._tint_slider.setEnabled(False)
        self._tint_lbl.setEnabled(False)

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

        lay.addWidget(create_separator())

        # ── Image ─────────────────────────────────────────────────────────────
        self._nr_combo = NoScrollComboBox()
        for label, _ in _NR_MODES:
            self._nr_combo.addItem(label)
        self._nr_combo.setCurrentIndex(1)  # Fast
        self._nr_combo.currentIndexChanged.connect(self._on_nr_mode_changed)
        lay.addLayout(_row("Noise Red.", self._nr_combo))

        self._edge_combo = NoScrollComboBox()
        for label, _ in _EDGE_MODES:
            self._edge_combo.addItem(label)
        self._edge_combo.setCurrentIndex(1)  # Fast
        self._edge_combo.currentIndexChanged.connect(self._on_edge_mode_changed)
        lay.addLayout(_row("Sharpening", self._edge_combo))

        img_row = QHBoxLayout()
        img_row.setContentsMargins(0, 0, 0, 0)
        il = QLabel("")
        il.setFixedWidth(110)
        img_row.addWidget(il)
        self._bll_cb = QCheckBox("Black level lock")
        self._bll_cb.toggled.connect(self._on_bll_changed)
        img_row.addWidget(self._bll_cb)
        img_row.addStretch()
        self._torch_btn = QPushButton("Torch")
        self._torch_btn.setCheckable(True)
        self._torch_btn.toggled.connect(self._on_torch_toggled)
        img_row.addWidget(self._torch_btn)
        lay.addLayout(img_row)

        return card

    # ── Stream lifecycle ──────────────────────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        self._lens_panel.set_placeholder("Loading lenses...")
        self._push_settings_to_phone()

    def _push_settings_to_phone(self):
        """Re-applies the widget state (already restored from the device's saved
        config by _apply_device_profile(), which runs independently of and before
        this) to the phone - on_stream_start() only wires up self._ctrl, it
        doesn't by itself make the phone match what the desktop already has
        loaded. Without this, the phone keeps whatever settings it booted with
        until the user touches a control again."""
        if self._manual_exp:
            self._ctrl.send(action="iso",     value=int(self._iso_slider.get_value()))
            self._ctrl.send(action="shutter", value=int(self._sht_slider.get_value()))
        else:
            self._ctrl.send(action="auto")
        if self._manual_wb:
            self._send_wb_gains()
        else:
            self._ctrl.send(action="wb_auto")
        self._ctrl.send(action="ois", value="1" if self._ois_cb.isChecked() else "0")
        if self._manual_focus:
            self._ctrl.send(action="focus_mode", value="manual")
            self._ctrl.send(action="focus_distance",
                            value=self._slider_to_diopters(self._focus_slider.value()))
        else:
            self._ctrl.send(action="focus_mode", value="continuous")
        self._ctrl.send(action="ae_comp", value=self._ae_comp_slider.value())
        self._ctrl.send(action="nr_mode", value=_NR_MODES[self._nr_combo.currentIndex()][1])
        self._ctrl.send(action="edge_mode", value=_EDGE_MODES[self._edge_combo.currentIndex()][1])
        self._ctrl.send(action="black_level_lock", value="1" if self._bll_cb.isChecked() else "0")

    def on_stream_stop(self):
        self._ctrl = None
        self._lens_panel.clear()
        self._cam_info_lbl.setText("")

    def on_phone_state(self, state: dict):
        view = derive_camera_control_view(state)
        if view is None:
            self._lens_panel.set_placeholder("Unavailable")
            return

        self._lens_panel.load(view.lenses)

        cur = view.current_camera
        if cur:
            self._iso_slider.set_range(cur.get("isoMin", 50), cur.get("isoMax", 6400))
            self._sht_slider.set_range(
                cur.get("shutterMinNs", 100_000),
                cur.get("shutterMaxNs", 1_000_000_000),
            )
            self._ae_comp_step = view.ae_comp_step
            self._ae_comp_slider.blockSignals(True)
            self._ae_comp_slider.setRange(*view.ae_comp_range)
            self._ae_comp_slider.blockSignals(False)
            self._update_cam_info_lbl(cur)
            self._update_camera_caps(
                cur.get("supportsManualSensor", True),
                cur.get("supportsManualWB", True),
                cur.get("supportsManualFocus", False),
                float(cur.get("minFocusDistance", 10.0)),
                cur.get("supportsFlash", False),
                cur.get("hasOis", True),
            )

        self._rb_exp_auto.setChecked(not view.manual_exposure)
        self._rb_exp_manual.setChecked(view.manual_exposure)
        self._manual_exp = view.manual_exposure
        self._iso_slider.set_enabled(view.manual_exposure)
        self._sht_slider.set_enabled(view.manual_exposure)
        if view.iso: self._iso_slider.set_value(float(view.iso))
        if view.shutter_ns: self._sht_slider.set_value(float(view.shutter_ns))

        self._rb_wb_auto.setChecked(not view.manual_wb)
        self._rb_wb_manual.setChecked(view.manual_wb)
        self._manual_wb = view.manual_wb
        self._wb_slider.setEnabled(view.manual_wb)
        self._wb_k_lbl.setEnabled(view.manual_wb)
        self._tint_slider.setEnabled(view.manual_wb)
        self._tint_lbl.setEnabled(view.manual_wb)

        self._ois_cb.setChecked(view.ois)

        self._rb_focus_auto.setChecked(not view.manual_focus)
        self._rb_focus_manual.setChecked(view.manual_focus)
        self._manual_focus = view.manual_focus
        self._focus_slider.setEnabled(view.manual_focus)
        self._set_focus_slider_value(view.focus_distance)

        self._ae_comp_slider.blockSignals(True)
        self._ae_comp_slider.setValue(view.ae_comp)
        self._ae_comp_slider.blockSignals(False)
        self._ae_comp_lbl.setText(f"{view.ae_comp * self._ae_comp_step:+.1f} EV")

        self._nr_combo.blockSignals(True)
        self._nr_combo.setCurrentIndex(view.nr_mode_index)
        self._nr_combo.blockSignals(False)

        self._edge_combo.blockSignals(True)
        self._edge_combo.setCurrentIndex(view.edge_mode_index)
        self._edge_combo.blockSignals(False)

        self._bll_cb.blockSignals(True)
        self._bll_cb.setChecked(view.black_level_lock)
        self._bll_cb.blockSignals(False)

        self._torch_btn.blockSignals(True)
        self._torch_btn.setChecked(view.torch)
        self._torch_on = view.torch
        self._torch_btn.blockSignals(False)

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
                            supports_manual_focus: bool = False, min_focus_distance: float = 10.0,
                            supports_flash: bool = False, has_ois: bool = True):
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
            self._wb_slider.setEnabled(False)
            self._wb_k_lbl.setEnabled(False)
            self._tint_slider.setEnabled(False)
            self._tint_lbl.setEnabled(False)
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

        self._torch_btn.setEnabled(supports_flash)
        if not supports_flash:
            self._torch_btn.setToolTip("This camera does not have a flash/torch")
        else:
            self._torch_btn.setToolTip("")

        # Greyed out (but left checked) on a lens without OIS: the phone still
        # remembers "OIS desired" and applies it as soon as an OIS-capable lens is
        # selected again, without needing to be re-toggled here.
        self._ois_cb.setEnabled(has_ois)
        self._ois_cb.setToolTip("" if has_ois else "This lens does not support optical image stabilization")

    # ── Handlers ──────────────────────────────────────────────────────────────

    def _on_lens_selected(self, cam: dict):
        if self._ctrl:
            self._ctrl.send(action="camera", id=cam["id"])
            self._iso_slider.set_range(cam.get("isoMin", 50), cam.get("isoMax", 6400))
            self._sht_slider.set_range(
                cam.get("shutterMinNs", 100_000),
                cam.get("shutterMaxNs", 1_000_000_000),
            )
            self._ae_comp_step = float(cam.get("aeCompStep", 0.167))
            ae_min = cam.get("aeCompMin", -8)
            ae_max = cam.get("aeCompMax", 8)
            self._ae_comp_slider.blockSignals(True)
            self._ae_comp_slider.setRange(ae_min, ae_max)
            self._ae_comp_slider.blockSignals(False)
            self._update_cam_info_lbl(cam)
            self._update_camera_caps(
                cam.get("supportsManualSensor", True),
                cam.get("supportsManualWB", True),
                cam.get("supportsManualFocus", False),
                float(cam.get("minFocusDistance", 10.0)),
                cam.get("supportsFlash", False),
                cam.get("hasOis", True),
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
        self._host.schedule_save()

    def _on_iso_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="iso", value=int(val))
        self._host.schedule_save()

    def _on_shutter_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="shutter", value=int(val))
        self._host.schedule_save()

    def _send_wb_gains(self):
        k = self._wb_slider.value()
        t = self._tint_slider.value()
        r, ge, go, b = _kelvin_to_rggb(k, float(t))
        self._ctrl.send(action="wb_gains", r=r, ge=ge, go=go, b=b)

    def _on_wb_mode(self):
        manual = self._rb_wb_manual.isChecked()
        self._manual_wb = manual
        self._wb_slider.setEnabled(manual)
        self._wb_k_lbl.setEnabled(manual)
        self._tint_slider.setEnabled(manual)
        self._tint_lbl.setEnabled(manual)
        if self._ctrl:
            if not manual:
                self._ctrl.send(action="wb_auto")
            else:
                self._send_wb_gains()
        self._host.schedule_save()

    def _on_wb_changed(self, k: int):
        self._wb_k_lbl.setText(f"{k} K")
        if self._ctrl and self._manual_wb:
            self._send_wb_gains()
        self._host.schedule_save()

    def _on_tint_changed(self, t: int):
        self._tint_lbl.setText(str(t))
        if self._ctrl and self._manual_wb:
            self._send_wb_gains()
        self._host.schedule_save()

    def _on_ois(self, checked: bool):
        if self._ctrl:
            self._ctrl.send(action="ois", value="1" if checked else "0")
        self._host.schedule_save()

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
        self._host.schedule_save()

    def _on_focus_slider(self, pos: int):
        d = self._slider_to_diopters(pos)
        self._focus_val_lbl.setText(_diopters_to_label(d))
        if self._ctrl and self._manual_focus:
            self._ctrl.send(action="focus_distance", value=d)
        self._host.schedule_save()

    def _on_ae_comp_changed(self, steps: int):
        ev = steps * self._ae_comp_step
        self._ae_comp_lbl.setText(f"{ev:+.1f} EV")
        if self._ctrl:
            self._ctrl.send(action="ae_comp", value=steps)
        self._host.schedule_save()

    def _on_nr_mode_changed(self, idx: int):
        if self._ctrl:
            self._ctrl.send(action="nr_mode", value=_NR_MODES[idx][1])
        self._host.schedule_save()

    def _on_edge_mode_changed(self, idx: int):
        if self._ctrl:
            self._ctrl.send(action="edge_mode", value=_EDGE_MODES[idx][1])
        self._host.schedule_save()

    def _on_bll_changed(self, checked: bool):
        if self._ctrl:
            self._ctrl.send(action="black_level_lock", value="1" if checked else "0")
        self._host.schedule_save()

    def _on_torch_toggled(self, checked: bool):
        self._torch_on = checked
        if self._ctrl:
            self._ctrl.send(action="torch", value="1" if checked else "0")

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
            "wb_manual":       self._rb_wb_manual.isChecked(),
            "wb_kelvin":       self._wb_slider.value(),
            "wb_tint":         self._tint_slider.value(),
            "ae_comp":         self._ae_comp_slider.value(),
            "nr_mode":         _NR_MODES[self._nr_combo.currentIndex()][1],
            "edge_mode":       _EDGE_MODES[self._edge_combo.currentIndex()][1],
            "bll":             self._bll_cb.isChecked(),
        }

    def set_config(self, cfg: dict):
        manual_exp = bool(cfg.get("exp_manual", False))
        self._rb_exp_manual.setChecked(manual_exp)
        self._rb_exp_auto.setChecked(not manual_exp)
        self._manual_exp = manual_exp
        self._iso_slider.set_enabled(manual_exp)
        self._sht_slider.set_enabled(manual_exp)
        if iso := cfg.get("iso"):
            self._iso_slider.set_value(float(iso))
        if sht := cfg.get("shutter_ns"):
            self._sht_slider.set_value(float(sht))
        self._ois_cb.setChecked(cfg.get("ois", True))
        manual_focus = bool(cfg.get("focus_manual", False))
        self._rb_focus_manual.setChecked(manual_focus)
        self._rb_focus_auto.setChecked(not manual_focus)
        self._manual_focus = manual_focus
        if d := cfg.get("focus_diopters"):
            self._set_focus_slider_value(float(d))
        manual_wb = bool(cfg.get("wb_manual", False))
        self._rb_wb_manual.setChecked(manual_wb)
        self._rb_wb_auto.setChecked(not manual_wb)
        self._manual_wb = manual_wb
        self._wb_slider.setEnabled(manual_wb)
        self._wb_k_lbl.setEnabled(manual_wb)
        self._tint_slider.setEnabled(manual_wb)
        self._tint_lbl.setEnabled(manual_wb)
        if k := cfg.get("wb_kelvin"):
            self._wb_slider.blockSignals(True)
            self._wb_slider.setValue(int(k))
            self._wb_slider.blockSignals(False)
            self._wb_k_lbl.setText(f"{int(k)} K")
        if (t := cfg.get("wb_tint")) is not None:
            self._tint_slider.setValue(int(t))
            self._tint_lbl.setText(str(int(t)))
        if (ae := cfg.get("ae_comp")) is not None:
            self._ae_comp_slider.setValue(int(ae))
            self._ae_comp_lbl.setText(f"{int(ae) * self._ae_comp_step:+.1f} EV")
        if (nr := cfg.get("nr_mode")) is not None:
            idx = next((i for i, (_, v) in enumerate(_NR_MODES) if v == nr), 1)
            self._nr_combo.setCurrentIndex(idx)
        if (em := cfg.get("edge_mode")) is not None:
            idx = next((i for i, (_, v) in enumerate(_EDGE_MODES) if v == em), 1)
            self._edge_combo.setCurrentIndex(idx)
        self._bll_cb.setChecked(bool(cfg.get("bll", False)))
