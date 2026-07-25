import math

from PyQt6.QtCore import QPoint, QRect, QSize, Qt, pyqtSignal
from PyQt6.QtGui import (
    QBrush, QColor, QFontMetrics, QIcon, QPainter, QPen, QPixmap,
)
from PyQt6.QtWidgets import (
    QComboBox, QDoubleSpinBox, QFrame, QHBoxLayout, QLabel, QLayout,
    QSlider, QSpinBox, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import theme


# ── Shared desktop UI primitives ─────────────────────────────────────────────

FORM_LABEL_WIDTH = 104

VALUE_COL_WIDTH = 62
"""Width of the numeric readout beside a slider. Fixed and right-aligned so
readouts line up down a panel instead of drifting with their text."""

SPIN_COL_WIDTH = 88
"""Width of the direct-entry spinbox beside a slider."""

SPIN_COL_GUTTER = SPIN_COL_WIDTH + 8
"""What a slider row without a spinbox reserves on its right, so its track
ends on the same line as the rows that have one."""

SLIDER_TRACK_WIDTH = 104
"""Minimum width for a slider track, not a fixed one.

Panels live in resizable columns now, so sliders stretch to whatever their
row has left over and this is only the floor below which the track stops
being usefully draggable. Apply it with `stretch_slider()`."""


def stretch_slider(slider: QWidget, minimum: int = SLIDER_TRACK_WIDTH) -> QWidget:
    """Let a slider grow with its column while keeping a draggable minimum."""
    slider.setMinimumWidth(minimum)
    slider.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
    return slider


class ElidingLabel(QLabel):
    """A label that shortens its text with an ellipsis instead of forcing
    its row wider.

    Plain QLabel reports its full text width as a minimum, so one long
    status message or capability list can stop a whole column from ever
    being narrowed again. This keeps the full text available as a tooltip.
    """

    def __init__(self, text: str = "", parent=None,
                 mode: Qt.TextElideMode = Qt.TextElideMode.ElideRight):
        super().__init__(parent)
        self._full = ""
        self._mode = mode
        self.setMinimumWidth(1)
        self.setSizePolicy(QSizePolicy.Policy.Ignored, QSizePolicy.Policy.Preferred)
        self.setText(text)

    def setText(self, text: str):
        self._full = text
        self.setToolTip(text if text else "")
        self._apply_elide()

    def fullText(self) -> str:
        return self._full

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._apply_elide()

    def _apply_elide(self):
        metrics = QFontMetrics(self.font())
        width = max(self.width(), 1)
        super().setText(metrics.elidedText(self._full, self._mode, width))


class FlowLayout(QLayout):
    """Lays widgets out left to right, wrapping to a new line when the row
    runs out of width.

    Qt ships nothing like this. A grid with a fixed column count can't cope
    with items whose natural widths differ (lens names run from "~15mm" to
    "~22mm OIS"), so it either truncates the long ones or forces the whole
    column wider than the rail. Wrapping sizes each item to its content and
    lets the count per row fall out of the space available.
    """

    def __init__(self, parent=None, spacing: int = 6, uniform: bool = False):
        super().__init__(parent)
        self._items: list = []
        self._spacing = spacing
        # uniform: give every item the same width and divide each row evenly,
        # so the rows end flush instead of trailing off wherever the last
        # item happened to finish. Reads as a grid whose column count adapts,
        # rather than a pile of differently-sized pills.
        self._uniform = uniform
        self.setContentsMargins(0, 0, 0, 0)

    def addItem(self, item):        self._items.append(item)
    def count(self):                return len(self._items)
    def itemAt(self, i):            return self._items[i] if 0 <= i < len(self._items) else None
    def takeAt(self, i):            return self._items.pop(i) if 0 <= i < len(self._items) else None
    def expandingDirections(self):  return Qt.Orientation(0)
    def hasHeightForWidth(self):    return True
    def heightForWidth(self, width): return self._layout(QRect(0, 0, width, 0), apply=False)

    def setGeometry(self, rect):
        super().setGeometry(rect)
        self._layout(rect, apply=True)

    def sizeHint(self):
        return self.minimumSize()

    def minimumSize(self):
        size = QSize()
        for item in self._items:
            size = size.expandedTo(item.minimumSize())
        m = self.contentsMargins()
        return size + QSize(m.left() + m.right(), m.top() + m.bottom())

    def _layout(self, rect: QRect, apply: bool) -> int:
        """Place every item and return the total height used. With
        apply=False this is the heightForWidth measurement pass."""
        if not self._items:
            return 0

        m = self.contentsMargins()
        left = rect.x() + m.left()
        y = rect.y() + m.top()
        avail = max(rect.width() - m.left() - m.right(), 1)

        if self._uniform:
            widest = max(i.sizeHint().width() for i in self._items)
            height = max(i.sizeHint().height() for i in self._items)
            per_row = max(1, min(len(self._items),
                                 (avail + self._spacing) // (widest + self._spacing)))
            width = (avail - (per_row - 1) * self._spacing) // per_row
            for index, item in enumerate(self._items):
                row, col = divmod(index, per_row)
                if apply:
                    item.setGeometry(QRect(
                        left + col * (width + self._spacing),
                        y + row * (height + self._spacing),
                        width, height))
            rows = -(-len(self._items) // per_row)
            return rows * height + (rows - 1) * self._spacing + m.top() + m.bottom()

        x = left
        line_height = 0
        for item in self._items:
            hint = item.sizeHint()
            if x > left and x + hint.width() > left + avail:
                x = left
                y += line_height + self._spacing
                line_height = 0
            if apply:
                item.setGeometry(QRect(QPoint(x, y), hint))
            x += hint.width() + self._spacing
            line_height = max(line_height, hint.height())

        return y + line_height - rect.y() + m.bottom()


def set_ui_role(widget: QWidget, role: str):
    """Apply a semantic visual role defined by the application QSS."""
    widget.setProperty("uiRole", role)
    style = widget.style()
    style.unpolish(widget)
    style.polish(widget)


def make_segmented(*buttons: QWidget):
    """Style a run of radio buttons or checkboxes as one joined pill strip.

    Purely presentational - the widgets stay exactly what they were, so
    QButtonGroup exclusivity and every existing signal connection are
    untouched. Each button gets a `segPos` so the stylesheet knows which
    corners to round and which inner borders to drop.

    Callers are expected to lay the buttons out with zero spacing, otherwise
    the segments read as separate pills with gaps between them.
    """
    last = len(buttons) - 1
    for i, btn in enumerate(buttons):
        if len(buttons) == 1:  pos = "only"
        elif i == 0:           pos = "first"
        elif i == last:        pos = "last"
        else:                  pos = "mid"
        btn.setProperty("segmented", True)
        btn.setProperty("segPos", pos)
        style = btn.style()
        style.unpolish(btn)
        style.polish(btn)
    return buttons


def segmented_row(*buttons: QWidget) -> QHBoxLayout:
    """A zero-spacing layout holding a segmented run.

    No trailing stretch: the enclosing `control_row` decides whether the
    strip fills the row or sits flush right, and a spacer in here would
    override that.
    """
    make_segmented(*buttons)
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(0)
    for btn in buttons:
        lay.addWidget(btn)
    return lay


def create_card(parent=None) -> QFrame:
    card = QFrame(parent)
    card.setObjectName("card")
    card.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred)
    return card


def add_card_header(layout: QVBoxLayout, title: str, icon_name: str,
                    subtitle: str = "") -> QHBoxLayout:
    """Add the standard icon/title header used by every main-window card."""
    header = QHBoxLayout()
    header.setContentsMargins(0, 0, 0, 4)
    header.setSpacing(9)

    icon = QLabel()
    icon.setPixmap(create_vector_icon(icon_name, theme.ACCENT).pixmap(18, 18))
    icon.setFixedSize(18, 18)
    header.addWidget(icon)

    # Uppercased here rather than at each call site so panel titles read as
    # section headers without every plugin having to shout in its source.
    title_label = QLabel(title.upper())
    title_label.setObjectName("card_title")
    header.addWidget(title_label)

    if subtitle:
        subtitle_label = QLabel(subtitle)
        subtitle_label.setObjectName("card_subtitle")
        header.addWidget(subtitle_label)

    header.addStretch()
    layout.addLayout(header)
    return header


def add_section_heading(layout: QVBoxLayout, text: str):
    heading = QLabel(text)
    heading.setObjectName("section_title")
    layout.addWidget(heading)
    return heading


def form_label(text: str, width: int = FORM_LABEL_WIDTH) -> QLabel:
    label = QLabel(text)
    label.setObjectName("form_label")
    label.setFixedWidth(width)
    label.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
    return label


def control_row(label: str, widget, label_width: int = FORM_LABEL_WIDTH,
                stretch: bool = False) -> QHBoxLayout:
    """The standard settings row: a dim label on the left, the control on
    the right.

    Both edges of the row are anchored, which is what keeps a panel from
    looking ragged: `stretch=True` grows the control to the card's right
    edge (sliders, combos), `stretch=False` pushes a naturally-sized
    control flush against it (spinboxes, segmented strips). Nothing floats
    in the middle with dead space either side of it.

    `widget` may be a widget or a layout. An empty label still reserves the
    label column, so a control can hang under the one above it without
    breaking the alignment grid.
    """
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    lay.addWidget(form_label(label, label_width))
    if not stretch:
        lay.addStretch(1)
    if isinstance(widget, QLayout): lay.addLayout(widget, 1 if stretch else 0)
    else:                           lay.addWidget(widget, 1 if stretch else 0)
    return lay


def control_row_widget(label: str, widget, label_width: int = FORM_LABEL_WIDTH,
                       stretch: bool = False) -> QWidget:
    """A hideable `control_row` - for rows revealed by a mode toggle."""
    container = QWidget()
    container.setObjectName("form_row")
    container.setLayout(control_row(label, widget, label_width, stretch))
    return container


def add_form_row(layout: QVBoxLayout, text: str, control: QWidget,
                 label_width: int = FORM_LABEL_WIDTH):
    """Add a consistently aligned label/control row to a card."""
    row = QHBoxLayout()
    row.setContentsMargins(0, 0, 0, 0)
    row.setSpacing(10)
    row.addWidget(form_label(text, label_width))
    row.addWidget(control, 1)
    layout.addLayout(row)
    return row

# ── Pure display helpers ──────────────────────────────────────────────────────


def ns_to_display(ns: int) -> str:
    if ns <= 0: return "?"
    s = ns / 1_000_000_000.0
    if s >= 1.0:
        return f"{s:.1f} s"
    denom = round(1.0 / s)
    return f"1/{denom:,}"


def quality_label(q: int) -> str:
    if q >= 95: return f"{q}%  High"
    if q >= 80: return f"{q}%  Balanced"
    if q >= 60: return f"{q}%  Low"
    return f"{q}%  Very low"


# ── Log-scale math ────────────────────────────────────────────────────────────

def log_pos_to_val(pos: int, steps: int, v_min: float, v_max: float) -> float:
    if v_min <= 0: v_min = 1
    t = pos / max(steps, 1)
    val = math.exp(math.log(v_min) + t * (math.log(v_max) - math.log(v_min)))
    return max(v_min, min(v_max, val))


def val_to_log_pos(val: float, steps: int, v_min: float, v_max: float) -> int:
    if val <= 0 or v_min <= 0: return 0
    val = max(v_min, min(v_max, val))
    t   = (math.log(val) - math.log(v_min)) / (math.log(v_max) - math.log(v_min))
    return round(t * steps)


# ── No-scroll subclasses ──────────────────────────────────────────────────────

class NoScrollComboBox(QComboBox):
    def wheelEvent(self, event):
        event.ignore()


class NoScrollSlider(QSlider):
    def wheelEvent(self, event):
        event.ignore()


class NoScrollSpinBox(QSpinBox):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setButtonSymbols(QSpinBox.ButtonSymbols.NoButtons)

    def wheelEvent(self, event):
        event.ignore()


class NoScrollDoubleSpinBox(QDoubleSpinBox):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setButtonSymbols(QDoubleSpinBox.ButtonSymbols.NoButtons)

    def wheelEvent(self, event):
        event.ignore()


# ── Widget factory helpers ────────────────────────────────────────────────────

def create_separator() -> QFrame:
    sep = QFrame()
    sep.setFrameShape(QFrame.Shape.HLine)
    sep.setObjectName("separator")
    return sep


def create_vector_icon(icon_name: str, color_hex: str) -> QIcon:
    pixmap = QPixmap(32, 32)
    pixmap.fill(Qt.GlobalColor.transparent)

    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)

    color = QColor(color_hex)
    pen = QPen(color)
    pen.setWidth(2)
    pen.setCapStyle(Qt.PenCapStyle.RoundCap)
    pen.setJoinStyle(Qt.PenJoinStyle.RoundJoin)
    painter.setPen(pen)
    painter.setBrush(QBrush(Qt.BrushStyle.NoBrush))

    if icon_name == "connection":
        painter.drawRoundedRect(11, 10, 10, 12, 2, 2)
        painter.drawLine(5, 13, 11, 13)
        painter.drawLine(5, 19, 11, 19)
        painter.drawLine(21, 16, 27, 16)
    elif icon_name == "camera":
        painter.drawRoundedRect(6, 11, 20, 13, 2, 2)
        painter.drawEllipse(12, 13, 8, 8)
        painter.drawRect(10, 8, 5, 3)
    elif icon_name == "stream":
        painter.drawRoundedRect(5, 8, 22, 14, 2, 2)
        painter.drawLine(16, 22, 16, 26)
        painter.drawLine(11, 26, 21, 26)
    elif icon_name == "gear":
        # Outer ring + teeth, larger to fill 32x32 canvas
        painter.drawEllipse(8, 8, 16, 16)
        painter.drawEllipse(12, 12, 8, 8)
        for i in range(8):
            painter.save()
            painter.translate(16, 16)
            painter.rotate(i * 45)
            painter.drawLine(0, -7, 0, -11)
            painter.restore()
    elif icon_name == "status":
        painter.drawEllipse(7, 7, 18, 18)
        pen_dot = QPen(color)
        pen_dot.setWidth(3)
        painter.setPen(pen_dot)
        painter.drawPoint(16, 12)
        painter.setPen(pen)
        painter.drawLine(16, 15, 16, 20)
    elif icon_name == "qr":
        brush = QBrush(color)
        # corner brackets
        painter.drawLine(4, 4, 4, 11)
        painter.drawLine(4, 4, 11, 4)
        painter.drawLine(28, 4, 21, 4)
        painter.drawLine(28, 4, 28, 11)
        painter.drawLine(4, 28, 4, 21)
        painter.drawLine(4, 28, 11, 28)
        painter.drawLine(28, 28, 21, 28)
        painter.drawLine(28, 28, 28, 21)
        # three small finder squares
        for ox, oy in [(8, 8), (18, 8), (8, 18)]:
            painter.drawRect(ox, oy, 6, 6)
            painter.fillRect(ox + 2, oy + 2, 2, 2, brush)
    elif icon_name == "usb":
        # connector body with two contacts on the left, cable to the right
        painter.drawRoundedRect(6, 13, 14, 8, 1, 1)
        painter.drawLine(6, 15, 3, 15)
        painter.drawLine(6, 19, 3, 19)
        painter.drawLine(20, 17, 24, 17)
        painter.drawLine(24, 17, 24, 7)
        painter.drawLine(24, 7, 28, 7)
    elif icon_name == "logo":
        # The app mark, matching the tray icon: an aperture ring with a
        # filled centre.
        painter.drawEllipse(4, 4, 24, 24)
        painter.setBrush(QBrush(color))
        painter.drawEllipse(12, 12, 8, 8)
        painter.setBrush(QBrush(Qt.BrushStyle.NoBrush))
    elif icon_name == "play":
        painter.setBrush(QBrush(color))
        from PyQt6.QtGui import QPolygon
        from PyQt6.QtCore import QPoint
        painter.drawPolygon(QPolygon([QPoint(10, 7), QPoint(25, 16), QPoint(10, 25)]))
        painter.setBrush(QBrush(Qt.BrushStyle.NoBrush))
    elif icon_name == "stop":
        painter.setBrush(QBrush(color))
        painter.drawRoundedRect(9, 9, 14, 14, 2, 2)
        painter.setBrush(QBrush(Qt.BrushStyle.NoBrush))
    elif icon_name == "expand":
        for x1, y1, x2, y2 in ((6, 12, 6, 6), (6, 6, 12, 6), (26, 12, 26, 6),
                               (26, 6, 20, 6), (6, 20, 6, 26), (6, 26, 12, 26),
                               (26, 20, 26, 26), (26, 26, 20, 26)):
            painter.drawLine(x1, y1, x2, y2)
    elif icon_name == "reset":
        # Circular arrow: an arc left open at the top right, with a head.
        painter.drawArc(8, 8, 16, 16, 60 * 16, 280 * 16)
        painter.drawLine(24, 12, 24, 6)
        painter.drawLine(24, 12, 18, 12)
    elif icon_name == "transforms":
        painter.drawLine(7, 11, 25, 11)
        painter.drawLine(7, 21, 25, 21)
        painter.drawLine(20, 7, 25, 11)
        painter.drawLine(20, 15, 25, 11)
        painter.drawLine(12, 17, 7, 21)
        painter.drawLine(12, 25, 7, 21)

    painter.end()
    return QIcon(pixmap)


# ── Log-scale slider row ──────────────────────────────────────────────────────

class LogSliderRow(QWidget):
    """Horizontal slider on log scale with spinbox for direct entry.

    spinbox_scale: multiply internal value by this for spinbox display.
    e.g. spinbox_scale=1e-6 shows nanoseconds as milliseconds.
    """
    value_changed = pyqtSignal(float)
    STEPS = 2000

    def __init__(self, v_min: float, v_max: float,
                 display_fn=None, spinbox_suffix: str = "",
                 spinbox_scale: float = 1.0,
                 spinbox_decimals: int = 0, parent=None):
        super().__init__(parent)
        self.setObjectName("inline_control")
        self.v_min = v_min
        self.v_max = v_max
        self.display_fn = display_fn or str
        self._spin_scale = spinbox_scale


        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(0, self.STEPS)
        self._slider.setValue(0)
        stretch_slider(self._slider)
        lay.addWidget(self._slider, 1)

        self._val_lbl = QLabel(display_fn(v_min) if display_fn else str(v_min))
        self._val_lbl.setObjectName("val")
        self._val_lbl.setFixedWidth(VALUE_COL_WIDTH)
        self._val_lbl.setAlignment(
            Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        lay.addWidget(self._val_lbl)

        self._is_double_spin = spinbox_decimals > 0
        if self._is_double_spin:
            spin = NoScrollDoubleSpinBox()
            spin.setDecimals(spinbox_decimals)
            spin.setRange(v_min * spinbox_scale, v_max * spinbox_scale)
            spin.setSingleStep(10 ** -spinbox_decimals)
        else:
            spin = NoScrollSpinBox()
            spin.setRange(int(v_min * spinbox_scale), int(v_max * spinbox_scale))
        spin.setSuffix(spinbox_suffix)
        spin.setFixedWidth(SPIN_COL_WIDTH)
        self._spin = spin
        lay.addWidget(self._spin)

        self._slider.valueChanged.connect(self._on_slider)
        self._spin.editingFinished.connect(self._on_spin)

    def _to_spin(self, val: float):
        sv = val * self._spin_scale
        return sv if self._is_double_spin else int(round(sv))

    def _on_slider(self, pos: int):
        val = log_pos_to_val(pos, self.STEPS, self.v_min, self.v_max)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))
        self._spin.blockSignals(True)
        self._spin.setValue(self._to_spin(val))
        self._spin.blockSignals(False)
        self._schedule_emit(val)

    def _on_spin(self):
        val = float(self._spin.value()) / self._spin_scale
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))
        self._schedule_emit(val)

    def _schedule_emit(self, val: float):
        self.value_changed.emit(val)

    def set_range(self, v_min: float, v_max: float):
        self.v_min = v_min
        self.v_max = v_max
        lo, hi = self._to_spin(v_min), self._to_spin(v_max)
        self._spin.setRange(lo, hi)
        cur_pos = self._slider.value()
        val = log_pos_to_val(cur_pos, self.STEPS, v_min, v_max)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))

    def get_value(self) -> float:
        return log_pos_to_val(self._slider.value(), self.STEPS, self.v_min, self.v_max)

    def set_value(self, val: float):
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        self._spin.blockSignals(True)
        self._spin.setValue(self._to_spin(val))
        self._spin.blockSignals(False)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
        self._spin.setEnabled(enabled)


# ── WB slider row ─────────────────────────────────────────────────────────────

# ── Pan slider row ────────────────────────────────────────────────────────────

class PanSliderRow(QWidget):
    """Linear slider -1.0 to 1.0 with a centered zero tick."""
    value_changed = pyqtSignal(float)
    STEPS = 200

    def __init__(self, label_neg: str = "L", label_pos: str = "R",
                 show_end_labels: bool = True, parent=None):
        super().__init__(parent)
        self.setObjectName("inline_control")
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        if show_end_labels:
            neg_lbl = QLabel(label_neg)
            neg_lbl.setObjectName("dim")
            lay.addWidget(neg_lbl)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(-self.STEPS, self.STEPS)
        self._slider.setValue(0)
        stretch_slider(self._slider)
        lay.addWidget(self._slider, 1)

        if show_end_labels:
            pos_lbl = QLabel(label_pos)
            pos_lbl.setObjectName("dim")
            lay.addWidget(pos_lbl)
        else:
            self.setMinimumWidth(SLIDER_TRACK_WIDTH)

        self._slider.valueChanged.connect(self._on_slider)

    def _on_slider(self, pos: int):
        self.value_changed.emit(pos / self.STEPS)

    def get_value(self) -> float:
        return self._slider.value() / self.STEPS

    def set_value(self, val: float):
        self._slider.blockSignals(True)
        self._slider.setValue(int(val * self.STEPS))
        self._slider.blockSignals(False)

    def reset(self):
        self.set_value(0.0)

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
