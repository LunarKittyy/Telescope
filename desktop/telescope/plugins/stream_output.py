import math

from PyQt6.QtCore import Qt, QTimer
from PyQt6.QtGui import QBrush, QColor
from PyQt6.QtWidgets import (
    QApplication, QButtonGroup, QComboBox, QStyle, QStyledItemDelegate, QStyleOptionViewItem, QWidget,
)

from telescope import h264_reader
from telescope.plugin import TelescopePlugin
from telescope.theme import OK, WARN
from telescope.widgets.banner import BannerAction, Issue
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, SegmentButton, add_card_header,
    add_section_heading, control_row as _row, control_row_widget, card_layout, create_card, dim_until_paired,
    quality_label, segmented_row, slider_row, value_label, wrapped_note,
)

_DEFAULT_QUALITY = 85  # the recommended spot, marked on the slider
_MAX_QUALITY     = 95  # past about 92 a JPEG roughly doubles in size for no difference anyone sees
_DEFAULT_FPS     = 30
_FPS_CHOICES     = (15, 24, 25, 30, 48, 60)
_MAX_BITRATE_MBPS = 100  # the phone clamps to the same range, and to what its encoder takes; 0 is Auto
# One past the top of the slider: Dynamic, as much as the connection carries. -1 in the config and to the phone, which
# an older desktop reads as Auto and an older phone treats as Auto.
_DYNAMIC_POS = _MAX_BITRATE_MBPS + 1
_DYNAMIC = -1
_BITRATE_TIP = ("Auto: about 8 Mbps at 1080p30. All the way right is Dynamic: as much as the connection carries, "
                "lowered before it lags.")
_NO_DYNAMIC_TIP = " This phone's app is too old for Dynamic and uses Auto until it's updated."

FORMAT_MJPEG = "mjpeg"
FORMAT_H264  = "h264"

# Shown as Light and Heavy: the codec names say nothing about the choice people actually make, which is how much data.
_FORMAT_NOTES = {
    FORMAT_H264:  "Good for most calls.",
    FORMAT_MJPEG: "Needs USB or strong Wi-Fi.",
}
# Tooltips carry the technical side; the note under the buttons is the plain one.
_LIGHT_TIP = "H.264 from the phone's hardware encoder, like video calls use."
_HEAVY_TIP = "MJPEG: every frame a full JPEG. Several times Light's data, sharper in fast motion."

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


def _parse_size_label(text):
    """(w, h) from a _size_label, or None."""
    try:
        w, h = (int(part) for part in str(text).split(" x "))
    except (TypeError, ValueError):
        return None
    return (w, h) if w > 0 and h > 0 else None


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


def _closest_size(wh: tuple, sizes) -> tuple:
    """The size nearest wh: its aspect ratio first, then the nearest height, the smaller one on a tie."""
    ratio = _aspect_ratio(*wh)
    return min(sizes, key=lambda s: (_aspect_ratio(*s) != ratio, abs(s[1] - wh[1]), s[0] * s[1]))


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
        # Last resolution this device used or picked; survives stream stop and device switches.
        self._saved_resolution_text = None
        self._format = FORMAT_H264
        self._phone_codecs: tuple = ()  # what the phone reported; () until it has
        self._phone_dynamic = None  # whether the phone takes Dynamic; None until it has reported
        self._phone_picked = True  # another source (Browser camera) says what helps itself
        self._h264_sizes = None  # the sizes the lens's H.264 encoder takes, as a set; None until a phone says
        self._good_size = None  # the last size a Light stream ran at without trouble, to go back to when one fails
        # Lens switch doesn't trigger fresh /v1/state fetch; use cached capabilities dict.
        bus.camera_switched.connect(self._on_camera_switched)
        bus.device_changed.connect(self._on_device_changed)
        bus.stream_behind.connect(self._on_stream_behind)
        bus.source_selected.connect(self._on_source_selected)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, "Stream output", "stream")
        dim_until_paired(card, self._bus, phone_only=True)

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
        # Capture and playback rate; faster just wastes power. Rates past what the lens lists are grayed out.
        self._fps_combo = NoScrollComboBox()
        for fps in _FPS_CHOICES:
            self._fps_combo.addItem(f"{fps} fps", fps)
        self._fps_combo.setToolTip("The phone's capture rate and the virtual camera's. Lower saves data and battery.")
        self._fps_combo.activated.connect(self._on_fps_picked)
        self._wanted_fps = _DEFAULT_FPS  # what was picked; a lens that can't do it runs the fastest it can below
        self._camera_max_fps = 0         # the current lens's fastest listed rate; 0 = not known, nothing grayed
        self._show_fps()
        lay.addLayout(_row("FPS", self._fps_combo, stretch=True))

        # ── JPEG Quality ──────────────────────────────────────────────────────
        add_section_heading(lay, "Phone stream")
        self._quality_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._quality_slider.setRange(1, _MAX_QUALITY)
        self._quality_slider.set_snaps([_DEFAULT_QUALITY])
        self._quality_slider.setValue(_DEFAULT_QUALITY)
        self._quality_slider.set_default(_DEFAULT_QUALITY)
        self._quality_val_lbl = value_label()
        self._show_quality(_DEFAULT_QUALITY)
        self._quality_slider.valueChanged.connect(self._on_quality_changed)
        self._quality_row = control_row_widget(
            "JPEG quality", slider_row(self._quality_slider, self._quality_val_lbl), stretch=True)
        lay.addWidget(self._quality_row)

        self._bitrate_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._bitrate_slider.setRange(0, _DYNAMIC_POS)
        self._bitrate_slider.setValue(0)
        self._bitrate_slider.set_default(0)  # Auto
        self._bitrate_val_lbl = value_label()
        self._bitrate_slider.setToolTip(_BITRATE_TIP)
        self._show_bitrate()
        self._bitrate_slider.valueChanged.connect(self._on_bitrate_changed)
        self._bitrate_row = control_row_widget(
            "Bitrate", slider_row(self._bitrate_slider, self._bitrate_val_lbl), stretch=True)
        lay.addWidget(self._bitrate_row)

        self._fmt_h264 = SegmentButton("Light")
        self._fmt_mjpeg = SegmentButton("Heavy")
        self._fmt_mjpeg.setToolTip(_HEAVY_TIP)
        self._fmt_grp = QButtonGroup(card)
        self._fmt_grp.setExclusive(True)
        for btn in (self._fmt_h264, self._fmt_mjpeg):
            self._fmt_grp.addButton(btn)
        self._fmt_h264.setChecked(True)
        self._fmt_grp.buttonClicked.connect(self._on_format_clicked)
        self._fmt_note = wrapped_note("")
        at = lay.indexOf(self._quality_row)
        lay.insertLayout(at, _row("Format", segmented_row(self._fmt_h264, self._fmt_mjpeg), stretch=True))
        lay.insertLayout(at + 1, _row("", self._fmt_note, stretch=True))
        self._show_format()

        # Goes with the card, so a push due after the window closed never reaches deleted controls
        self._initial_timer = QTimer(card)
        self._initial_timer.setSingleShot(True)
        self._initial_timer.timeout.connect(self._push_initial_settings)
        return card

    def get_stream_params(self) -> tuple:
        """Return (width, height, fps) for StreamWorker (width/height always None; resolution controlled by phone)."""
        return None, None, self._fps()

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        self._initial_timer.start(1500)

    def on_stream_stop(self):
        self._initial_timer.stop()
        if self._res_combo.currentData() is not None:
            self._saved_resolution_text = self._res_combo.currentText()
            # The lens's sizes stay up, so a size can be picked before starting again (say, after one was too much for
            # the encoder); the next start opens at it, and the phone's state fills the lists in afresh.
            self._pending_resolution_text = self._saved_resolution_text
        self._ctrl = None
        self._current_camera_id = None

    def _forget_sizes(self):
        """Another phone: this one's sizes and rates mean nothing there."""
        self._h264_sizes = None
        self._good_size = None
        self._camera_max_fps = 0
        self._show_fps()
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
            self._ctrl.send(action="bitrate", value=self._bitrate_bps())
            self._ctrl.send(action="fps_target",   value=self._fps())

    def on_phone_state(self, state: dict):
        # {} means the state fetch failed, not a phone without H.264 (a phone leaves codecs out when it's only MJPEG)
        codecs = tuple(state.get("codecs") or (FORMAT_MJPEG,)) if "cameras" in state else self._phone_codecs
        if codecs and codecs != self._phone_codecs:
            self._phone_codecs = codecs
            self._show_format()
        if "cameras" in state:
            self._phone_dynamic = bool(state.get("dynamic_bitrate"))
            self._show_bitrate()
        back = None
        if self._format == FORMAT_H264 and codecs and FORMAT_H264 not in codecs:
            # Light is the default, and a phone without an encoder sends nothing on its route: Heavy for this phone.
            self._set_format(FORMAT_MJPEG)
        elif self._format == FORMAT_H264 and state.get("codec_error") and state.get("codec_unsupported"):
            # Too much for the phone's encoder at this size or rate. Heavy there can be hundreds of Mbps, so rather
            # than switch by itself, stop and let the choice be made: something smaller, or Heavy anyway. The stop
            # waits for every plugin to have this state, so none fills its controls back in after it.
            sid = self._host.focused_source_id()  # the panels may show another stream by the time it's answered
            QTimer.singleShot(0, lambda: self._host.stop_stream(sid))
            back = self._back_to_good_size(state)
            actions = [BannerAction("Switch to Heavy", self._host.start_again(sid, self._switch_to_heavy))]
            if back:
                text = f"{state['codec_error']}. It's set back to {back}, the last size that worked."
                actions.insert(0, BannerAction(f"Start at {back}", self._host.start_again(sid)))
            else:
                text = f"{state['codec_error']}. Try a lower resolution or FPS, or switch to Heavy."
            self._host.show_issue("encoder", Issue("Too much for the phone's H.264 encoder", text, actions, kind="warn"))
        elif self._format == FORMAT_H264 and state.get("codec_error"):
            # The phone went back to MJPEG; so does the stream, or it would keep asking for H.264.
            self._host.show_issue("h264", Issue(
                "Switched to Heavy", f"{state['codec_error']}. You can try Light again in Stream output.", kind="warn"))
            self._set_format(FORMAT_MJPEG)
        cams = state.get("cameras")
        if not isinstance(cams, list):
            return
        cur = next((c for c in cams if c.get("current")), None)
        if cur is None:
            return
        live = (state.get("stream_width"), state.get("stream_height"))
        if back:
            live = (None, None)  # the size it failed at isn't the one to show any more
        if state.get("codec") == FORMAT_H264 and not state.get("codec_error") and all(isinstance(v, int) for v in live):
            self._good_size = live
        self._set_camera_max_fps(cur.get("maxFps"))
        self._apply_camera(cur, *live)

    def _back_to_good_size(self, state: dict):
        """Puts the size back to the last one Light ran at, so the next start doesn't fail the same way; its label."""
        failed = (state.get("stream_width"), state.get("stream_height"))
        good = self._good_size
        if good is None or good == failed or self._res_combo.findText(_size_label(*good)) < 0:
            return None
        self._select_resolution(good)
        self._saved_resolution_text = self._pending_resolution_text = _size_label(*good)
        self._host.schedule_save()
        return _size_label(*good)

    def _light_sizes(self, cam: dict):
        """The sizes this lens's H.264 encoder takes, as a set; None when the phone doesn't say (older app, no encoder)."""
        listed = cam.get("h264Sizes")
        if not isinstance(listed, list):
            return None
        return {(s["width"], s["height"]) for s in listed if isinstance(s, dict) and "width" in s and "height" in s}

    def _on_camera_switched(self, cam: dict):
        current = self._res_combo.currentData()  # Lens switch reuses ImageReader; resolution carries over.
        live_w, live_h = current if current else (None, None)
        self._apply_camera(cam, live_w, live_h)

    def _apply_camera(self, cam: dict, live_w, live_h):
        sizes = cam.get("supportedSizes") or []
        sizes = [(s["width"], s["height"]) for s in sizes
                 if isinstance(s, dict) and "width" in s and "height" in s]
        self._h264_sizes = self._light_sizes(cam)
        if self.stream_format() == FORMAT_H264 and self._h264_sizes:
            sizes = [wh for wh in sizes if wh in self._h264_sizes] or sizes  # leave out what Light can't do
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
            final_wh = self._res_combo.currentData()  # Whatever _select_resolution actually landed on.
            # Push it whenever it isn't what the phone runs at (a default, or a lens without the old size),
            # or the box would show and save a size the phone never got.
            if final_wh is not None and final_wh != (live_w, live_h) and (force_default or (live_w and live_h)):
                self._on_resolution()

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
        """Point both combos at `wh` (the nearest listed size if it isn't listed, the top entry if None) without
        notifying the phone."""
        listed = [s for sizes in self._sizes_by_ratio.values() for s in sizes]
        if wh and listed and wh not in listed:
            wh = _closest_size(wh, listed)  # a size Light can't do, or another lens's
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
        current = self._res_combo.currentData()
        self._rebuild_resolution_combo(ratio)
        if current:
            # The size nearest the current height, not the largest: that can be more than the phone's encoder takes
            idx = self._res_combo.findText(_size_label(*_closest_size(current, self._sizes_by_ratio[ratio])))
            self._res_combo.blockSignals(True)
            self._res_combo.setCurrentIndex(max(idx, 0))
            self._res_combo.blockSignals(False)
        self._on_resolution()  # Switching AR is itself a resolution change; notify like any other.

    def opening(self) -> dict:
        """What the phone should open at when a stream starts: the size picked here (if known) and the FPS."""
        out = {"fps": self._fps()}
        wh = _parse_size_label(self._pending_resolution_text or self._saved_resolution_text)
        if wh and self.stream_format() == FORMAT_H264 and self._h264_sizes and wh not in self._h264_sizes:
            wh = _closest_size(wh, self._h264_sizes)  # picked for Heavy: Light opens at the nearest it can do
        if wh:
            out["width"], out["height"] = wh
        return out

    def _on_resolution(self):
        size = self._res_combo.currentData()
        if size is None:
            return
        if self._ctrl is None:  # picked while stopped: the next start opens at it
            self._saved_resolution_text = self._pending_resolution_text = self._res_combo.currentText()
            self._host.schedule_save()
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
        self._forget_sizes()
        self._phone_codecs = ()  # another phone: unknown until it reports
        self._phone_dynamic = None
        self._show_format()
        self._show_bitrate()

    def _h264_offered(self) -> bool:
        """Until the phone has reported, assume it can: nearly every phone has an encoder, and one without falls back."""
        return h264_reader.available() and (not self._phone_codecs or FORMAT_H264 in self._phone_codecs)

    def _show_format(self):
        h264 = self.stream_format() == FORMAT_H264
        for btn, on in ((self._fmt_mjpeg, not h264), (self._fmt_h264, h264)):
            btn.blockSignals(True)
            btn.setChecked(on)
            btn.blockSignals(False)
        self._fmt_h264.setEnabled(self._h264_offered())
        if not h264_reader.available():
            tip = "H.264. Needs PyAV on this computer (pip install av)."
        elif self._phone_codecs and FORMAT_H264 not in self._phone_codecs:
            tip = "H.264. This phone has no H.264 encoder."
        else:
            tip = _LIGHT_TIP
        self._fmt_h264.setToolTip(tip)
        self._fmt_note.setText(_FORMAT_NOTES[FORMAT_H264 if h264 else FORMAT_MJPEG])
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

    def _on_source_selected(self, sid: str):
        self._phone_picked = not sid

    def _on_stream_behind(self, behind: bool):
        if not self._phone_picked:
            return
        if not behind:
            self._host.clear_issue("behind")
            return
        on_light = self.stream_format() == FORMAT_H264
        if not on_light and self._h264_offered():
            text, action = "Try Light or lower quality.", BannerAction("Switch to Light", self._switch_to_light)
        elif on_light and self._dynamic():
            text, action = "Try a lower resolution or FPS.", None  # Dynamic already lowered what it could
        elif on_light and self._phone_dynamic is not False:
            text, action = "Try Dynamic or lower quality.", BannerAction("Switch to Dynamic", self._switch_to_dynamic)
        else:
            text, action = "Try lower quality.", None
        self._host.show_issue("behind", Issue("Can't keep up", text, [action] if action else [], kind="warn"))

    def _switch_to_light(self):
        self._host.clear_issue("h264")
        self._set_format(FORMAT_H264)

    def _switch_to_heavy(self):
        self._set_format(FORMAT_MJPEG)

    def _switch_to_dynamic(self):
        self._bitrate_slider.setValue(_DYNAMIC_POS)  # sends it and saves, as moving the slider there would

    def _dynamic(self) -> bool:
        return self._bitrate_slider.value() == _DYNAMIC_POS

    def _bitrate_bps(self) -> int:
        return _DYNAMIC if self._dynamic() else self._bitrate_slider.value() * 1_000_000

    def _show_bitrate(self):
        mbps = self._bitrate_slider.value()
        self._bitrate_val_lbl.setText("Dynamic" if self._dynamic() else f"{mbps} Mbps" if mbps else "Auto")
        self._bitrate_slider.setToolTip(_BITRATE_TIP + (_NO_DYNAMIC_TIP if self._phone_dynamic is False else ""))

    def _on_bitrate_changed(self, _value: int):
        self._show_bitrate()
        if self._ctrl:
            self._ctrl.send(action="bitrate", value=self._bitrate_bps())
        self._host.schedule_save()

    def _fps(self) -> int:
        return self._fps_combo.currentData() or _DEFAULT_FPS

    @staticmethod
    def _nearest_fps(fps) -> int:
        """A saved rate (older versions took any 5-60) as the closest choice, the lower one on a tie."""
        try:
            fps = int(fps)
        except (TypeError, ValueError):
            return _DEFAULT_FPS
        return min(_FPS_CHOICES, key=lambda c: (abs(c - fps), c))

    def _show_fps(self) -> bool:
        """Gray out rates past the lens's and select the picked one, or the fastest it can below. True if the rate
        in use changed."""
        before = self._fps_combo.currentData()
        limit = self._camera_max_fps
        model = self._fps_combo.model()
        for i, fps in enumerate(_FPS_CHOICES):
            model.item(i).setEnabled(not limit or fps <= limit)
        usable = [fps for fps in _FPS_CHOICES if not limit or fps <= limit] or [_FPS_CHOICES[0]]
        use = max((fps for fps in usable if fps <= self._wanted_fps), default=usable[0])
        self._fps_combo.setCurrentIndex(_FPS_CHOICES.index(use))
        return before is not None and use != before

    def _set_camera_max_fps(self, max_fps):
        max_fps = max_fps if isinstance(max_fps, int) and max_fps > 0 else 0
        if max_fps == self._camera_max_fps:
            return
        self._camera_max_fps = max_fps
        if self._show_fps():
            self._apply_fps()  # the picked rate stays saved, for a lens that can do it

    def _on_fps_picked(self, _index: int):
        self._wanted_fps = self._fps()
        self._apply_fps()
        self._host.schedule_save()

    def _apply_fps(self):
        fps = self._fps()
        self._host.update_stream_output(fps=fps)
        if self._ctrl:
            self._ctrl.send(action="fps_target", value=fps)

    def _show_quality(self, q: int):
        self._quality_val_lbl.setText(f"{q}%")
        tip = (f"{quality_label(q)}. The dot ({_DEFAULT_QUALITY}%) is recommended: higher costs a lot more data "
               "for little you'd see. On slow Wi-Fi, lower it or the FPS.")
        self._quality_slider.setToolTip(tip)
        self._quality_val_lbl.setToolTip(tip)

    def _on_quality_changed(self, q: int):
        self._show_quality(q)
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=q)
        self._host.schedule_save()

    # ── Config ────────────────────────────────────────────────────────────────

    def diagnostics(self) -> dict:
        return {
            "Format": self.stream_format() + (" (H.264 chosen)" if self._format != self.stream_format() else ""),
            "Phone codecs": ", ".join(self._phone_codecs) or "unknown",
            "Resolution": self._res_combo.currentText() or "auto",
            "FPS": str(self._fps()) + (f" ({self._wanted_fps} chosen)" if self._fps() != self._wanted_fps else ""),
            "Bitrate": self._bitrate_val_lbl.text() + (" (phone uses Auto)" if self._dynamic() and
                                                       self._phone_dynamic is False else ""),
        }

    def get_config(self) -> dict:
        cfg = {
            "fps":          self._wanted_fps,
            "jpeg_quality": self._quality_slider.value(),
            "format":       self._format,
            "bitrate_mbps": _DYNAMIC if self._dynamic() else self._bitrate_slider.value(),
        }
        if self._ctrl is not None and self._res_combo.currentData() is not None:
            self._saved_resolution_text = self._res_combo.currentText()  # stopped, a pick or preset already set it
        if self._saved_resolution_text:
            cfg["resolution"] = self._saved_resolution_text
        return cfg

    def apply_preset(self, cfg: dict):
        """Load a preset's settings and, while streaming, send them."""
        old_format = self.stream_format()
        self.set_config(cfg)
        if self._phone_codecs and FORMAT_H264 not in self._phone_codecs:
            self._format = FORMAT_MJPEG  # a preset without a format can't ask this phone for Light
            self._show_format()
        if not self._ctrl:
            wh = self._find_by_label(self._pending_resolution_text) if self._pending_resolution_text else None
            if wh:
                self._select_resolution(wh)  # the sizes still up from the last stream show it
            return
        if self.stream_format() != old_format:
            self._host.reconnect_stream()  # the new stream picks everything up as it starts
            return
        self._push_initial_settings()
        self._host.update_stream_output(fps=self._fps())
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
        if fps := cfg.get("fps"):
            self._wanted_fps = self._nearest_fps(fps)
            self._show_fps()
        if q := cfg.get("jpeg_quality"):
            self._quality_slider.setValue(int(q))
        self._format = FORMAT_MJPEG if cfg.get("format") == FORMAT_MJPEG else FORMAT_H264
        self._bitrate_slider.blockSignals(True)
        mbps = int(cfg.get("bitrate_mbps", 0) or 0)
        self._bitrate_slider.setValue(_DYNAMIC_POS if mbps == _DYNAMIC else max(0, min(_MAX_BITRATE_MBPS, mbps)))
        self._bitrate_slider.blockSignals(False)
        self._show_bitrate()
        self._show_format()
