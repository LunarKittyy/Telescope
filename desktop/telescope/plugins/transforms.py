import math
import time
from typing import Optional

from PyQt6.QtCore import QEvent, QRectF, Qt, QTimer
from PyQt6.QtGui import QColor, QPainter
from PyQt6.QtWidgets import (
    QButtonGroup, QHBoxLayout, QLabel, QWidget,
)

from telescope import theme
from telescope.view import (
    ROTATIONS, PhoneZoomCaps, ViewStatus, lens_step, pan_for, shown_to_view, window, window_centre, zoom_detent,
)
from telescope.widgets.controls import PanPad
from telescope.widgets.line_icons import line_icon
from telescope.plugin import StreamsBehind, TelescopePlugin
from telescope.shortcuts import button_action, choice_action, slider_action
from telescope.widgets.common import (
    set_button_icon,
    NoScrollComboBox, NoScrollSlider, PanSliderRow, SegmentButton, add_card_header,
    control_row as _row, card_layout, create_card, card_action,
    dim_until_paired, segmented_row, slider_row, ui_px, value_label,
)
from telescope.widgets.lens_panel import shorten_lens_label
from telescope.theme import ACCENT, ERR

# Rotation's segments, by the names saved settings and the shortcut use.
_ROTATION_SEGMENTS = (("None", "0°"), ("90 CW", "90°"), ("180", "180°"), ("90 CCW", "270°"))

_PAN_SNAP = 0.02  # a preview drag sticks at the centre within this much of the frame, on each axis
_LENS_SETTLE_S = 2.0  # how long the phone gets to take a new ratio before the dot says it didn't


def _number(value, default: float, lo: float, hi: float) -> float:
    # A hand-edited or damaged config value becomes the default instead of breaking every frame
    try:
        value = float(value)
    except (TypeError, ValueError):
        return default
    return min(hi, max(lo, value)) if math.isfinite(value) else default


def lens_note(zoom: float, sent_ratio: float, caps: Optional[PhoneZoomCaps], live: str, default: str,
              tele: str, settled: bool = True) -> tuple:
    """What the lens dot shows: (level, hover text). Level "" = nothing worth saying, so no dot;
    "on" = the phone is on a longer lens; "off" = it isn't on the lens this zoom would get.

    live/default/tele are lens names: the one streaming now, the one used unzoomed, and the last longer
    lens seen live. "Switched" goes by what the phone reports, not by what it was asked for. `settled` is
    whether the ratio sent last has had time to take, so a normal switch doesn't count as the phone refusing.
    """
    lines, level = [], ""
    if live and default and live != default:
        lines.append(f"Switched to {live}")
        level = "on"
    if caps is not None and caps.lens_zooms:
        centred = max((lens for lens in caps.lens_zooms if zoom >= lens), default=1.0)
        if centred > sent_ratio + 1e-3:
            lines.append(f"Panned past what {tele or 'the telephoto'} can see, so it's on "
                         f"{live or 'the main camera'}. Pan back to switch again.")
            level = "off"
        elif sent_ratio > 1.0 + 1e-3 and settled and live and live == default:
            lines.append(f"The phone stayed on {live} by itself, most likely too dark or too close "
                         f"for {tele or 'the telephoto'}. It switches once it can.")
            level = "off"
    if lines:
        lines.append("It can wobble for a moment while it switches.")
    return level, "\n".join(lines)



class _LensDot(QWidget):
    """A small painted dot just left of a value label's text, level with the middle of its digits.
    Lavender: on a longer lens; red: not on the lens this zoom would get.

    Painted rather than a "●" glyph, whose height inside the line differs from font to font. It's a
    child of the label, outside any layout, so showing it never moves anything.
    """

    _DIAMETER = 6
    _GAP = 4

    def __init__(self, label: QLabel):
        super().__init__(label)
        self._d = ui_px(self._DIAMETER)
        self.setFixedSize(self._d + 4, self._d + 4)  # a bit of slack around it makes it easier to hover
        self._color = QColor(ERR)
        label.installEventFilter(self)
        self.place()

    def set_level(self, level: str):
        """lens_note's level: "on", "off", or "" to hide."""
        self._color = QColor(ACCENT if level == "on" else ERR)
        self.setVisible(bool(level))
        self.update()

    def eventFilter(self, obj, event):
        if event.type() in (QEvent.Type.Resize, QEvent.Type.FontChange, QEvent.Type.StyleChange):
            self.place()
        return False

    def place(self):
        """Follow the label's text, which is right-aligned and vertically centred."""
        lbl = self.parentWidget()
        fm = lbl.fontMetrics()
        rect = lbl.contentsRect()
        baseline = rect.top() + (rect.height() - fm.height()) / 2 + fm.ascent()
        middle = baseline - fm.capHeight() / 2
        text_left = rect.right() + 1 - fm.horizontalAdvance(lbl.text())
        x = text_left - ui_px(self._GAP) - (self.width() + self._d) / 2
        self.move(max(0, round(x)), round(middle - self.height() / 2))

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(self._color)
        off = (self.width() - self._d) / 2
        p.drawEllipse(QRectF(off, off, self._d, self._d))




class TransformsPlugin(TelescopePlugin):
    name = "transforms"
    title = "Transforms"
    about = "Zoom, pan, flip and rotate the picture."
    when_off = "Without it there's no Transforms card and the picture goes out exactly as the phone sends it."
    device_local = True
    panel_region = "left"   # desktop-side processing, with the output settings

    def setup(self, host, bus):
        self._host = host
        self.flip_h   = False  # the manual view; the window's view (host.set_view) does the cropping
        self.flip_v   = False
        self.rotation = None
        self.zoom     = 1.0
        self.pan_x    = 0.0
        self.pan_y    = 0.0
        self._ctrl = None
        self._status = ViewStatus()  # how the zoom is shared with the phone (view_status)
        self._taken_by = ""  # what moves zoom and pan instead of this card ("" = nobody)
        self._zoom_where = ""  # the tooltip's first line: where the zoom happens (from the last split)
        self._live_lens = ""   # the lens a multi-lens camera streams from right now, as the lens picker names it
        self._default_lens = ""  # the lens it streams from unzoomed (seen live while the ratio was 1)
        self._tele_lens = ""     # the last longer lens seen live, to name it once it's out of view
        self._behind = StreamsBehind()  # the lens names of each stream the panels moved away from
        self._bus = bus
        bus.max_zoom_changed.connect(self._on_max_zoom)
        bus.view_dragged.connect(self._on_view_dragged)
        bus.view_scrolled.connect(self._on_view_scrolled)
        bus.view_status.connect(self._on_view_status)
        bus.view_taken.connect(self._on_view_taken)
        self._raw_pan = None  # a preview drag's pan before the centre snap (see _on_view_dragged)
        self._wheel_zoom = 1.0  # the scroll wheel's zoom, finer than the slider's 0.01 steps (trackpads)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._reset_btn = card_action("Reset", "reset", "Clear flip, rotation, zoom and pan back to defaults")
        self._reset_btn.clicked.connect(self._reset_all)
        add_card_header(lay, "Transforms", "crop", action=self._reset_btn, hue="transforms")
        dim_until_paired(card, self._bus)

        # ── Flip ─────────────────────────────────────────────────────────────
        self._flip_h = SegmentButton("Mirror")
        self._flip_v = SegmentButton("Upside down")
        set_button_icon(self._flip_h, line_icon("mirror", theme.TEXT_DIM), 13)
        set_button_icon(self._flip_v, line_icon("upside_down", theme.TEXT_DIM), 13)
        self._flip_h.toggled.connect(self._on_flip)
        self._flip_v.toggled.connect(self._on_flip)
        lay.addLayout(_row("Flip", segmented_row(self._flip_h, self._flip_v), stretch=True))

        # ── Rotation ──────────────────────────────────────────────────────────
        self._rot_combo = NoScrollComboBox()
        self._rot_combo.addItems(list(ROTATIONS.keys()))
        self._rot_combo.currentTextChanged.connect(self._on_rotate)
        self._rot_combo.setVisible(False)  # the segments drive it; it stays for shortcuts and saved settings
        self._rot_segs = {}
        rot_grp = QButtonGroup(card)
        for name, text in _ROTATION_SEGMENTS:
            btn = SegmentButton(text)
            btn.setProperty("shortcutValue", name)
            btn.clicked.connect(lambda _c=False, n=name: self._rot_combo.setCurrentText(n))
            rot_grp.addButton(btn)
            self._rot_segs[name] = btn
        self._rot_segs["None"].setChecked(True)
        self._rot_combo.currentTextChanged.connect(lambda n: self._rot_segs[n].setChecked(True))
        rot_row = _row("Rotation", segmented_row(*self._rot_segs.values()), stretch=True)
        rot_row.addWidget(self._rot_combo)
        lay.addLayout(rot_row)

        # ── Zoom ──────────────────────────────────────────────────────────────
        self._zoom_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._zoom_slider.setRange(100, 1000)  # up to Max zoom in Advanced (max_zoom_changed)
        self._zoom_slider.setValue(100)
        self._zoom_slider.set_default(100)
        self._zoom_val_lbl = value_label("1.0×")
        self._zoom_slider.valueChanged.connect(self._on_zoom_changed)
        lay.addLayout(_row("Zoom", slider_row(self._zoom_slider, self._zoom_val_lbl), stretch=True))
        # Only there when the phone switched lens (or isn't on the one it should be); details on hover.
        self._lens_dot = _LensDot(self._zoom_val_lbl)
        self._lens_dot.setVisible(False)
        # Checks the dot again once a new ratio had time to take, in case no phone state comes in between.
        self._settle_timer = QTimer(self._lens_dot)
        self._settle_timer.setSingleShot(True)
        self._settle_timer.setInterval(int(_LENS_SETTLE_S * 1000) + 100)
        self._settle_timer.timeout.connect(self._show_zoom_where)

        # ── Pan ───────────────────────────────────────────────────────────────
        # The pad is what shows; the two sliders stay hidden behind it for shortcuts.
        hidden = QWidget(card)
        hidden.setVisible(False)
        hidden_lay = QHBoxLayout(hidden)
        self._pan_x_slider = PanSliderRow(show_end_labels=False)
        self._pan_x_slider.value_changed.connect(self._on_pan_changed)
        self._pan_x_lbl = value_label()
        self._pan_y_slider = PanSliderRow(show_end_labels=False)
        self._pan_y_slider.value_changed.connect(self._on_pan_changed)
        self._pan_y_lbl = value_label()
        for w in (self._pan_x_slider, self._pan_x_lbl, self._pan_y_slider, self._pan_y_lbl):
            hidden_lay.addWidget(w)

        self._pan_pad = PanPad()
        self._pan_pad.moved.connect(self._on_pan_pad)
        self._pan_lbl = value_label("H 0%\nV 0%")
        pan_row = _row("Pan", slider_row(self._pan_pad, self._pan_lbl), stretch=True)
        pan_row.setAlignment(pan_row.itemAt(0).widget(), Qt.AlignmentFlag.AlignVCenter)
        lay.addLayout(pan_row)

        self._pan_x_slider.set_enabled(False)
        self._pan_y_slider.set_enabled(False)
        self._show_pan()

        return card

    def create_actions(self) -> list:
        g = "Transforms"
        actions = [
            button_action("transforms.flip_horizontal", "Flip horizontal", g, self._flip_h),
            button_action("transforms.flip_vertical", "Flip vertical", g, self._flip_v),
            choice_action("transforms.rotation", "Rotation", g, buttons=list(self._rot_segs.values())),
            slider_action("transforms.zoom", "Zoom", g, self._zoom_slider, scale=0.01, suffix="×", decimals=2,
                          step=0.25, readout=self._zoom_val_lbl),
            slider_action("transforms.pan_x", "Pan left/right", g, self._pan_x_slider.slider(), scale=0.5,
                          suffix="%", step=10.0, readout=self._pan_x_lbl, signed=True,
                          widgets=(self._pan_pad,), ready_widget=self._pan_pad),
            slider_action("transforms.pan_y", "Pan up/down", g, self._pan_y_slider.slider(), scale=0.5,
                          suffix="%", step=10.0, readout=self._pan_y_lbl, signed=True,
                          widgets=(self._pan_pad,), ready_widget=self._pan_pad),
            button_action("transforms.reset", "Reset transforms", g, self._reset_btn),
        ]
        keywords = {"transforms.flip_horizontal": "mirror", "transforms.flip_vertical": "mirror upside down",
                    "transforms.rotation": "rotate turn", "transforms.zoom": "crop framing",
                    "transforms.pan_x": "framing move", "transforms.pan_y": "framing move"}
        for action in actions:
            action.keywords = keywords.get(action.id, "")
        return actions


    def _reset_all(self):
        """Reset to defaults; handlers fire from widget changes."""
        self._flip_h.setChecked(False)
        self._flip_v.setChecked(False)
        self._rot_combo.setCurrentIndex(0)
        self._zoom_slider.setValue(100)
        self._pan_x_slider.reset()
        self._pan_y_slider.reset()
        self._on_pan_changed(0.0)

    # ── Which lens the phone is on ────────────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl

    def on_focus_gained(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        kept = self._behind.take(self._host, ctrl)
        if kept is not None:
            self._live_lens, self._default_lens, self._tele_lens = kept

    def on_stream_stop(self):
        self._behind.drop(self._host)
        self._ctrl = None
        self._live_lens = ""
        self._show_zoom_where()

    def on_focus_lost(self):
        self._behind.keep(self._host, self._ctrl, (self._live_lens, self._default_lens, self._tele_lens))
        self._ctrl = None
        self._live_lens = ""
        self._show_zoom_where()

    def on_phone_state(self, state: dict):
        cameras = state.get("cameras", [])
        live = next((c for c in cameras if c.get("id") == state.get("active_lens")), None)
        self._live_lens = shorten_lens_label(live.get("label", "")) if live else ""
        # Learnt only once the ratio sent last had time to take: right after a sweep back under a lens, or on a
        # reconnect, the phone can still be on the telephoto, and taking that for the unzoomed lens made the dot red.
        sent = self._status.sent
        settled = time.monotonic() - self._status.sent_ratio_at >= _LENS_SETTLE_S
        if self._live_lens and sent is not None and settled:
            if sent.ratio <= 1.0:
                self._default_lens = self._live_lens
            elif self._default_lens and self._live_lens != self._default_lens:
                self._tele_lens = self._live_lens
        self._show_zoom_where()

    def _on_view_status(self, status: ViewStatus):
        """The window's view split the zoom with the phone again (view_status)."""
        old, self._status = self._status, status
        if status.caps != old.caps:
            if status.caps is not None:
                self._default_lens = self._tele_lens = ""  # another camera: its lenses are learnt afresh
            # Dragging sticks to them: a lens's own ratio is the sharpest view it has.
            self._zoom_slider.set_snaps([lens_step(z) for z in status.caps.lens_zooms] if status.caps else [])
        if status.sent_ratio_at != old.sent_ratio_at:
            self._settle_timer.start()
        if status.sent is None:
            self._zoom_where = "Zoomed on this computer" + (": this lens can't zoom itself" if status.streaming else "")
        elif status.desktop_zoom > 1.0:
            self._zoom_where = "Zoomed on the phone's sensor as far as this lens allows, the rest on this computer"
        else:
            self._zoom_where = "Zoomed on the phone's sensor"
        self._show_zoom_where()

    def _show_zoom_where(self):
        where = self._zoom_where
        if self._zoom_slider.snaps():
            where += "\nDots mark where the phone switches to another lens"
        if self._taken_by:
            where = f"{self._taken_by} is moving the zoom and pan"
        self._zoom_slider.setToolTip(where)
        self._zoom_val_lbl.setToolTip(where)
        status = self._status
        streaming = self._ctrl is not None and status.streaming
        settled = time.monotonic() - status.sent_ratio_at >= _LENS_SETTLE_S
        level, note = lens_note(self.zoom, status.sent.ratio if status.sent else 1.0,
                                status.caps if streaming else None, self._live_lens if streaming else "",
                                self._default_lens, self._tele_lens, settled)
        self._lens_dot.setToolTip(note)
        self._lens_dot.set_level(level)

    def _on_view_taken(self, owner: str):
        self._taken_by = owner
        self._framing_enabled()
        self._show_zoom_where()

    # ── Handlers (Qt thread) ──────────────────────────────────────────────────

    def _on_max_zoom(self, max_zoom: int):
        self._zoom_slider.setMaximum(max_zoom * 100)  # a zoom past it is clamped, through _on_zoom_changed

    def _on_flip(self):
        self.flip_h = self._flip_h.isChecked()
        self.flip_v = self._flip_v.isChecked()
        self._host.set_view(flip_h=self.flip_h, flip_v=self.flip_v)
        self._host.schedule_save()

    def _on_rotate(self):
        self.rotation = ROTATIONS.get(self._rot_combo.currentText())
        self._host.set_view(rotation=self.rotation)
        self._host.schedule_save()

    def _on_zoom_changed(self, val: int):
        self.zoom = val / 100.0
        self._zoom_val_lbl.setText(f"{self.zoom:.1f}×")
        self._lens_dot.place()
        if self.zoom <= 1.0:
            self.pan_x = self.pan_y = 0.0
        self._framing_changed()

    def _on_pan_changed(self, _val: float):
        self.pan_x = self._pan_x_slider.get_value()
        self.pan_y = self._pan_y_slider.get_value()
        self._framing_changed()

    def _on_pan_pad(self, x: float, y: float):
        self._pan_x_slider.set_value(x)
        self._pan_y_slider.set_value(y)
        self._on_pan_changed(0.0)

    def _on_view_dragged(self, du: float, dv: float):
        """The preview was dragged by (du, dv) of the frame as shown: the picture follows the mouse."""
        if self.zoom <= 1.0 or self._taken_by:
            return
        # Where the drag alone would put the pan; what's shown sticks at the centre near it (_PAN_SNAP), and
        # dragging on past that lets go without a jump, since this keeps counting underneath.
        raw_x, raw_y = self._raw_pan or (self.pan_x, self.pan_y)
        t = (self.zoom, raw_x, raw_y, self.flip_h, self.flip_v, self.rotation)
        x0, y0 = shown_to_view(0.5, 0.5, *t)
        x1, y1 = shown_to_view(0.5 + du, 0.5 + dv, *t)
        raw_x = pan_for(window_centre(self.zoom, raw_x) - (x1 - x0), self.zoom)
        raw_y = pan_for(window_centre(self.zoom, raw_y) - (y1 - y0), self.zoom)
        off_centre = (self.zoom - 1.0) / 2  # a pan of 1, in frames as shown
        self.pan_x = 0.0 if abs(raw_x) * off_centre <= _PAN_SNAP else raw_x
        self.pan_y = 0.0 if abs(raw_y) * off_centre <= _PAN_SNAP else raw_y
        self._framing_changed(keep_raw=True)
        self._raw_pan = (raw_x, raw_y)

    def _on_view_scrolled(self, factor: float, u: float, v: float):
        """Zoom by `factor`, keeping the point under the mouse ((u, v) of the frame as shown) where it is."""
        if self._taken_by:
            return
        slider = self._zoom_slider
        old = slider.value()
        if abs(self._wheel_zoom * 100 - old) >= 1:
            self._wheel_zoom = old / 100.0  # moved some other way since the last scroll
        target = min(max(self._wheel_zoom * factor, 1.0), slider.maximum() / 100.0)
        new = zoom_detent(old, round(target * 100), slider.snaps())
        self._wheel_zoom = new / 100.0 if new != round(target * 100) else target
        if new == old:
            return
        # Where the point sits inside the window stays the same, so it stays under the mouse.
        t = (self.flip_h, self.flip_v, self.rotation)
        px, py = shown_to_view(u, v, self.zoom, self.pan_x, self.pan_y, *t)
        (x0, w), (y0, h) = window(self.zoom, self.pan_x), window(self.zoom, self.pan_y)
        zoom = new / 100.0
        size = 1.0 / zoom
        self.pan_x = pan_for(px - (px - x0) / w * size + size / 2, zoom)
        self.pan_y = pan_for(py - (py - y0) / h * size + size / 2, zoom)
        slider.setValue(new)  # the zoom path does the rest, with the pan set above

    def _framing_changed(self, keep_raw: bool = False):
        """Zoom or pan changed: the sliders follow and the window's view moves (the phone hears, the lens boxes move)."""
        if not keep_raw:
            self._raw_pan = None  # moved some other way: a drag starts from where the picture is
        for slider, pan in ((self._pan_x_slider, self.pan_x), (self._pan_y_slider, self.pan_y)):
            slider.set_value(pan)
        self._pan_pad.set_position(self.pan_x, self.pan_y)
        self._framing_enabled()
        self._host.set_view(zoom=self.zoom, pan_x=self.pan_x, pan_y=self.pan_y)
        self._show_zoom_where()
        self._host.schedule_save()

    def _framing_enabled(self):
        """Zoom and pan work by hand unless something else moves them; pan only once zoomed in."""
        free = not self._taken_by
        self._zoom_slider.setEnabled(free)
        pan_active = free and self.zoom > 1.0
        for slider in (self._pan_x_slider, self._pan_y_slider):
            slider.set_enabled(pan_active)
        self._pan_pad.setEnabled(pan_active)
        self._show_pan()

    def _show_pan(self):
        for lbl, slider, pan in ((self._pan_x_lbl, self._pan_x_slider, self.pan_x),
                                 (self._pan_y_lbl, self._pan_y_slider, self.pan_y)):
            pct = round(pan * 100)
            lbl.setText(f"{pct:+d}%" if pct else "0%")
            lbl.setEnabled(slider._slider.isEnabled())
        self._pan_lbl.setText(f"H {self._pan_x_lbl.text()}\nV {self._pan_y_lbl.text()}")
        self._pan_lbl.setEnabled(self._pan_pad.isEnabled())

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {
            "flip_h":    self._flip_h.isChecked(),
            "flip_v":    self._flip_v.isChecked(),
            "rotation":  self._rot_combo.currentText(),
            "zoom":      self._zoom_slider.value() / 100.0,
            "pan_x":     self.pan_x,  # finer than the sliders: dragging the preview sets it directly
            "pan_y":     self.pan_y,
        }

    def set_config(self, cfg: dict):
        self._flip_h.setChecked(cfg.get("flip_h", False))
        self._flip_v.setChecked(cfg.get("flip_v", False))
        if rot := cfg.get("rotation"):
            idx = self._rot_combo.findText(rot)
            if idx >= 0:
                self._rot_combo.setCurrentIndex(idx)
        zoom = _number(cfg.get("zoom", 1.0), 1.0, 1.0, 30.0)  # the slider holds it to Max zoom
        self._zoom_slider.setValue(round(zoom * 100))
        pan_active = zoom > 1.0
        self.pan_x = _number(cfg.get("pan_x", 0.0), 0.0, -1.0, 1.0) if pan_active else 0.0
        self.pan_y = _number(cfg.get("pan_y", 0.0), 0.0, -1.0, 1.0) if pan_active else 0.0
        self._framing_changed()
        # Unchanged widgets don't fire, so the window's view hears flip and rotation here too
        self._host.set_view(flip_h=self.flip_h, flip_v=self.flip_v, rotation=self.rotation)
