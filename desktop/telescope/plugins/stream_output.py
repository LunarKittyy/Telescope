import math

from PyQt6.QtCore import Qt, QTimer
from PyQt6.QtGui import QBrush, QColor
from PyQt6.QtWidgets import (
    QApplication, QButtonGroup, QComboBox, QStyle, QStyledItemDelegate, QStyleOptionViewItem, QWidget,
)

from telescope import h264_reader
from telescope.plugin import TelescopePlugin
from telescope.theme import OK, WARN
from telescope.widgets.banner import Issue
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, NoScrollSpinBox, SegmentButton, add_card_header,
    add_section_heading, control_row as _row, control_row_widget, card_layout, create_card,
    quality_label, segmented_row, slider_row, value_label,
)

_DEFAULT_QUALITY = 85
_DEFAULT_FPS     = 30
_MAX_BITRATE_MBPS = 30  # the phone clamps to the same range; 0 lets it size the bitrate itself

FORMAT_MJPEG = "mjpeg"
FORMAT_H264  = "h264"

# "1080p" etc. names a height, not one exact WxH - matching by height catches every ratio's version.
_COMMON_HEIGHTS = {2160, 1440, 1080, 720, 480, 360}  # 4K, 1440p, 1080p, 720p, 480p, 360p

_COMMON_ASPECT_RATIOS = {(16, 9), (4, 3)}

_PREFERRED_DEFAULTS = ((1920, 1080), (1280, 720))  # Preferred default, in priority order.
_COMMON_COLOR = QColor(OK)
_AR_COMMON_COLOR = QColor(WARN)  # Aspect ratios that contain a common resolution.


class _ColoredItemDelegate(QStyledItemDelegate):
    """Paints item text by hand - the app's QSS overrides Qt::ForegroundRole otherwise."""

    def paint(self, painter, option, index):
        opt = QStyleOptionViewItem(option)
        self.initStyleOption(opt, index)
        widget = opt.widget
        style = widget.style() if widget else QApplication.style()
        text = opt.text
        opt.text = ""
        style.drawControl(QStyle.ControlElement.CE_ItemViewItem, opt, painter, widget)

        brush = index.data(Qt.ItemDataRole.ForegroundRole)
        if isinstance(brush, QBrush):
            color = brush.color()
        elif isinstance(brush, QColor):
            color = brush
        else:
            color = opt.palette.text().color()

        painter.save()
        painter.setPen(color)
        font = index.data(Qt.ItemDataRole.FontRole)
        if font is not None:
            painter.setFont(font)
        text_rect = style.subElementRect(QStyle.SubElement.SE_ItemViewItemText, opt, widget)
        painter.drawText(text_rect, int(Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft), text)
        painter.restore()


def _size_label(w: int, h: int) -> str:
    return f"{w} x {h}"


def _aspect_ratio(w: int, h: int) -> tuple:
    """Exact reduction, snapped to a common ratio if within rounding distance (e.g. 854x480 -> 16:9)."""
    ratio = w / h
    for common in _COMMON_ASPECT_RATIOS:
        if abs(ratio - common[0] / common[1]) < 0.02:
            return common
    g = math.gcd(w, h)
    return (w // g, h // g)


def _ratio_label(ratio: tuple) -> str:
    return f"{ratio[0]}:{ratio[1]}"


class StreamOutputPlugin(TelescopePlugin):
    name = "stream_output"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._ctrl = None
        self._current_camera_id = None
        self._sizes_by_ratio = {}
        self._ratios_sorted = []
        # Set on set_config() before phone data arrives; applied once on_phone_state() has real sizes.
        self._pending_resolution_text = None
        self._had_saved_resolution = False  # True if this device has ever had a resolution saved.
        # Last resolution this device used; survives stream stop, which clears the combos.
        self._saved_resolution_text = None
        self._format = FORMAT_MJPEG
        self._phone_codecs: tuple = ()  # what the phone reported; () until it has
        # Lens switch doesn't trigger fresh /v1/state fetch; use cached capabilities dict.
        bus.camera_switched.connect(self._on_camera_switched)
        bus.device_changed.connect(self._on_device_changed)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, "Stream output", "stream")

        # ── Resolution ────────────────────────────────────────────────────────
        add_section_heading(lay, "Output")
        self._ar_combo = NoScrollComboBox()  # Aspect ratio; narrows the resolution list below.
        self._ar_combo.setItemDelegate(_ColoredItemDelegate(self._ar_combo))
        self._ar_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToContents)
        self._ar_combo.addItem("—")
        self._ar_combo.setEnabled(False)
        self._ar_combo.currentIndexChanged.connect(self._on_aspect_ratio_changed)
        lay.addLayout(_row("Aspect ratio", self._ar_combo, stretch=True))

        self._res_combo = NoScrollComboBox()
        self._res_combo.setItemDelegate(_ColoredItemDelegate(self._res_combo))
        self._res_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToContents)
        self._res_combo.addItem("—")
        self._res_combo.setEnabled(False)
        self._res_combo.currentIndexChanged.connect(self._on_resolution)
        lay.addLayout(_row("Resolution", self._res_combo, stretch=True))

        # ── FPS ───────────────────────────────────────────────────────────────
        self._fps_spin = NoScrollSpinBox()  # Capture and playback rate; faster just wastes power.
        self._fps_spin.setRange(5, 60)
        self._fps_spin.setValue(_DEFAULT_FPS)
        self._fps_spin.setSuffix(" fps")
        self._fps_spin.setToolTip("Both the phone's capture rate and the local virtual camera's "
                                   "playback rate. Lower reduces bandwidth and phone battery use.")
        self._fps_spin.editingFinished.connect(self._on_fps)
        lay.addLayout(_row("FPS", self._fps_spin, stretch=True))

        # ── JPEG Quality ──────────────────────────────────────────────────────
        add_section_heading(lay, "Phone stream")
        self._quality_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._quality_slider.setRange(1, 100)
        self._quality_slider.setValue(_DEFAULT_QUALITY)
        self._quality_val_lbl = value_label()
        self._show_quality(_DEFAULT_QUALITY)
        self._quality_slider.valueChanged.connect(self._on_quality_changed)
        self._quality_row = control_row_widget(
            "JPEG quality", slider_row(self._quality_slider, self._quality_val_lbl), stretch=True)
        lay.addWidget(self._quality_row)

        self._bitrate_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._bitrate_slider.setRange(0, _MAX_BITRATE_MBPS)
        self._bitrate_slider.setValue(0)
        self._bitrate_val_lbl = value_label()
        self._bitrate_slider.setToolTip("Auto picks about 8 Mbps for 1080p at 30 fps, less for smaller sizes.")
        self._show_bitrate(0)
        self._bitrate_slider.valueChanged.connect(self._on_bitrate_changed)
        self._bitrate_row = control_row_widget(
            "Bitrate", slider_row(self._bitrate_slider, self._bitrate_val_lbl), stretch=True)
        lay.addWidget(self._bitrate_row)

        self._fmt_mjpeg = SegmentButton("MJPEG")
        self._fmt_h264 = SegmentButton("H.264")
        self._fmt_grp = QButtonGroup(card)
        self._fmt_grp.setExclusive(True)
        for btn in (self._fmt_mjpeg, self._fmt_h264):
            self._fmt_grp.addButton(btn)
        self._fmt_mjpeg.setChecked(True)
        self._fmt_grp.buttonClicked.connect(self._on_format_clicked)
        lay.insertLayout(lay.indexOf(self._quality_row),
                         _row("Format", segmented_row(self._fmt_mjpeg, self._fmt_h264), stretch=True))
        self._show_format()

        return card

    def get_stream_params(self) -> tuple:
        """Return (width, height, fps) for StreamWorker (width/height always None; resolution controlled by phone)."""
        return None, None, self._fps_spin.value()

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        QTimer.singleShot(1500, self._push_initial_settings)

    def on_stream_stop(self):
        if self._res_combo.currentData() is not None:
            self._saved_resolution_text = self._res_combo.currentText()
        self._ctrl = None
        self._current_camera_id = None
        self._sizes_by_ratio = {}
        self._ratios_sorted = []
        for combo in (self._ar_combo, self._res_combo):
            combo.blockSignals(True)
            combo.clear()
            combo.addItem("—")
            combo.setEnabled(False)
            combo.blockSignals(False)

    def _push_initial_settings(self):
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=self._quality_slider.value())
            self._ctrl.send(action="bitrate", value=self._bitrate_slider.value() * 1_000_000)
            self._ctrl.send(action="fps_target",   value=self._fps_spin.value())

    def on_phone_state(self, state: dict):
        codecs = tuple(state.get("codecs") or (FORMAT_MJPEG,))
        if codecs != self._phone_codecs:
            self._phone_codecs = codecs
            self._show_format()
        if self._format == FORMAT_H264 and state.get("codec_error"):
            # The phone went back to MJPEG; so does the stream, or it would keep asking for H.264.
            self._host.show_issue("h264", Issue("Back to MJPEG", state["codec_error"] + ".", kind="warn"))
            self._set_format(FORMAT_MJPEG)
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
            is_first_bind = self._current_camera_id is None  # The very first camera this stream session has seen.
            self._current_camera_id = cam["id"]
            self._rebuild_camera_sizes(sizes)

            target_text = self._pending_resolution_text
            target_wh = self._find_by_label(target_text) if target_text else None
            # A brand-new device with nothing saved: pick our own preferred default rather than
            # trusting whatever the phone happened to already be at (its own unchecked guess).
            # Only on the session's first camera - a mid-session lens switch still carries over
            # live state below, same as a device that does have a saved preference.
            fresh_device = is_first_bind and target_wh is None and not self._had_saved_resolution
            if not fresh_device and target_wh is None and live_w and live_h:
                target_wh = (live_w, live_h)
            force_default = target_wh is None  # No saved preference and no live state - apply our own default.
            if force_default:
                target_wh = self._default_resolution()
            self._select_resolution(target_wh)
            if force_default:
                final_wh = self._res_combo.currentData()  # Whatever _select_resolution actually landed on.
                if final_wh is not None and final_wh != (live_w, live_h):
                    self._on_resolution()  # Actually push the default to the phone, not just show it.

            self._ar_combo.setEnabled(True)
            self._res_combo.setEnabled(True)
            self._pending_resolution_text = None
        elif live_w and live_h:
            live_text = _size_label(live_w, live_h)  # Same lens; reflect live size if changed.
            if self._res_combo.currentText() != live_text:
                self._select_resolution((live_w, live_h))

    def _default_resolution(self):
        """1080p16:9, else 720p16:9, else largest 16:9, else largest 4:3; None if neither ratio exists."""
        sixteen_nine = self._sizes_by_ratio.get((16, 9), ())
        for wh in _PREFERRED_DEFAULTS:
            if wh in sixteen_nine:
                return wh
        for ratio in ((16, 9), (4, 3)):
            sizes = self._sizes_by_ratio.get(ratio)
            if sizes:
                return max(sizes, key=lambda wh: wh[0] * wh[1])
        return None

    def _find_by_label(self, label: str):
        for sizes in self._sizes_by_ratio.values():
            match = next((wh for wh in sizes if _size_label(*wh) == label), None)
            if match:
                return match
        return None

    def _rebuild_camera_sizes(self, sizes: list):
        """Group sizes by aspect ratio and repopulate the AR combo, narrowest ratio first."""
        groups: dict = {}
        for w, h in sizes:
            groups.setdefault(_aspect_ratio(w, h), []).append((w, h))
        self._sizes_by_ratio = groups
        self._ratios_sorted = sorted(groups, key=lambda r: r[0] / r[1])

        self._ar_combo.blockSignals(True)
        self._ar_combo.clear()
        for ratio in self._ratios_sorted:
            self._ar_combo.addItem(_ratio_label(ratio), ratio)
            if ratio in _COMMON_ASPECT_RATIOS:
                idx = self._ar_combo.count() - 1
                font = self._ar_combo.font()
                font.setBold(True)
                self._ar_combo.setItemData(idx, font, Qt.ItemDataRole.FontRole)
                self._ar_combo.setItemData(idx, _AR_COMMON_COLOR, Qt.ItemDataRole.ForegroundRole)
        self._ar_combo.blockSignals(False)

    def _rebuild_resolution_combo(self, ratio: tuple):
        """Populate the resolution combo for one aspect ratio, largest first. Selects the first entry."""
        self._res_combo.blockSignals(True)
        self._res_combo.clear()
        for w, h in sorted(self._sizes_by_ratio[ratio], key=lambda wh: wh[0] * wh[1], reverse=True):
            self._res_combo.addItem(_size_label(w, h), (w, h))
            if h in _COMMON_HEIGHTS:
                idx = self._res_combo.count() - 1
                font = self._res_combo.font()
                font.setBold(True)
                self._res_combo.setItemData(idx, font, Qt.ItemDataRole.FontRole)
                self._res_combo.setItemData(idx, _COMMON_COLOR, Qt.ItemDataRole.ForegroundRole)
        self._res_combo.setCurrentIndex(0)
        self._res_combo.blockSignals(False)

    def _select_resolution(self, wh):
        """Point both combos at `wh` (or the top entry, if `wh` is None/unavailable) without notifying the phone."""
        ratio = _aspect_ratio(*wh) if wh else None
        if ratio not in self._sizes_by_ratio:
            ratio, wh = None, None
        ar_idx = self._ratios_sorted.index(ratio) if ratio else 0

        self._ar_combo.blockSignals(True)
        self._ar_combo.setCurrentIndex(ar_idx)
        self._ar_combo.blockSignals(False)

        self._rebuild_resolution_combo(self._ratios_sorted[ar_idx])

        if wh:
            idx = self._res_combo.findText(_size_label(*wh))
            if idx >= 0:
                self._res_combo.blockSignals(True)
                self._res_combo.setCurrentIndex(idx)
                self._res_combo.blockSignals(False)

    # ── Handlers ──────────────────────────────────────────────────────────────

    def _on_aspect_ratio_changed(self):
        ratio = self._ar_combo.currentData()
        if ratio is None:
            return
        self._rebuild_resolution_combo(ratio)  # Defaults to the group's largest size.
        self._on_resolution()  # Switching AR is itself a resolution change; notify like any other.

    def _on_resolution(self):
        size = self._res_combo.currentData()
        if size is None or self._ctrl is None:
            return
        w, h = size
        self._ctrl.send(action="resolution", width=w, height=h)
        self._bus.resolution_change_requested.emit(w, h)
        self._host.schedule_save()

    # ── Format ────────────────────────────────────────────────────────────────

    def stream_format(self) -> str:
        """The route to stream from: H.264 only when chosen and this computer can decode it."""
        return FORMAT_H264 if self._format == FORMAT_H264 and h264_reader.available() else FORMAT_MJPEG

    def _on_device_changed(self, _name: str):
        self._phone_codecs = ()  # another phone: unknown until it reports
        self._show_format()

    def _h264_offered(self) -> bool:
        return h264_reader.available() and FORMAT_H264 in self._phone_codecs

    def _show_format(self):
        h264 = self._format == FORMAT_H264
        for btn, on in ((self._fmt_mjpeg, not h264), (self._fmt_h264, h264)):
            btn.blockSignals(True)
            btn.setChecked(on)
            btn.blockSignals(False)
        self._fmt_h264.setEnabled(h264 or self._h264_offered())
        if not h264_reader.available():
            tip = "Needs PyAV on this computer (pip install av)."
        elif not self._phone_codecs:
            tip = "Start streaming to see whether the phone can send H.264."
        elif FORMAT_H264 not in self._phone_codecs:
            tip = "This phone has no H.264 encoder."
        else:
            tip = "Less bandwidth than MJPEG at the same quality. The phone's hardware encoder does the work."
        self._fmt_h264.setToolTip(tip)
        self._quality_row.setVisible(not h264)
        self._bitrate_row.setVisible(h264)

    def _on_format_clicked(self, btn):
        self._host.clear_issue("h264")
        self._set_format(FORMAT_H264 if btn is self._fmt_h264 else FORMAT_MJPEG)

    def _set_format(self, fmt: str):
        if fmt == self._format:
            return
        self._format = fmt
        self._show_format()
        self._host.schedule_save()
        # The route decides the phone's codec, so switching means reconnecting.
        self._host.reconnect_stream()

    def _show_bitrate(self, mbps: int):
        self._bitrate_val_lbl.setText(f"{mbps} Mbps" if mbps else "Auto")

    def _on_bitrate_changed(self, mbps: int):
        self._show_bitrate(mbps)
        if self._ctrl:
            self._ctrl.send(action="bitrate", value=mbps * 1_000_000)
        self._host.schedule_save()

    def _on_fps(self):
        fps = self._fps_spin.value()
        self._host.update_stream_output(fps=fps)
        if self._ctrl:
            self._ctrl.send(action="fps_target", value=fps)
        self._host.schedule_save()

    def _show_quality(self, q: int):
        self._quality_val_lbl.setText(f"{q}%")
        tip = (f"{quality_label(q)}. Lower quality and FPS reduce bandwidth, which helps on slow "
               "Wi-Fi or USB 2. Very low values are a last resort: the image gets blocky fast.")
        self._quality_slider.setToolTip(tip)
        self._quality_val_lbl.setToolTip(tip)

    def _on_quality_changed(self, q: int):
        self._show_quality(q)
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=q)
        self._host.schedule_save()

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        cfg = {
            "fps":          self._fps_spin.value(),
            "jpeg_quality": self._quality_slider.value(),
            "format":       self._format,
            "bitrate_mbps": self._bitrate_slider.value(),
        }
        if self._res_combo.currentData() is not None:
            self._saved_resolution_text = self._res_combo.currentText()
        if self._saved_resolution_text:
            cfg["resolution"] = self._saved_resolution_text
        return cfg

    def apply_preset(self, cfg: dict):
        """Load a preset's settings and, while streaming, send them."""
        old_format = self.stream_format()
        self.set_config(cfg)
        if not self._ctrl:
            return
        if self.stream_format() != old_format:
            self._host.reconnect_stream()  # the new stream picks everything up as it starts
            return
        self._push_initial_settings()
        self._host.update_stream_output(fps=self._fps_spin.value())
        wh = self._find_by_label(self._pending_resolution_text) if self._pending_resolution_text else None
        if wh and wh != self._res_combo.currentData():
            self._select_resolution(wh)
            self._on_resolution()
        if wh:
            self._pending_resolution_text = None

    def set_config(self, cfg: dict):
        # Always overwrite: the host applies defaults before each device's own config.
        res = cfg.get("resolution") or None
        self._pending_resolution_text = res  # Combo unpopulated at load; apply when on_phone_state() arrives.
        self._saved_resolution_text = res
        self._had_saved_resolution = res is not None
        if fps := cfg.get("fps", cfg.get("phone_fps")):  # Fallback to legacy "phone_fps" if "fps" absent.
            self._fps_spin.setValue(int(fps))
        if q := cfg.get("jpeg_quality"):
            self._quality_slider.setValue(int(q))
        self._format = FORMAT_H264 if cfg.get("format") == FORMAT_H264 else FORMAT_MJPEG
        self._bitrate_slider.blockSignals(True)
        self._bitrate_slider.setValue(max(0, min(_MAX_BITRATE_MBPS, int(cfg.get("bitrate_mbps", 0) or 0))))
        self._bitrate_slider.blockSignals(False)
        self._show_bitrate(self._bitrate_slider.value())
        self._show_format()
