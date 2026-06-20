import math
from typing import Optional

from PyQt6.QtCore import Qt, pyqtSignal, QTimer
from PyQt6.QtGui import QColor, QIcon, QPainter, QPen, QBrush, QPixmap
from PyQt6.QtWidgets import (
    QComboBox, QDoubleSpinBox, QFrame, QHBoxLayout, QLabel,
    QSlider, QSpinBox, QWidget,
)

# ── WB data ───────────────────────────────────────────────────────────────────

WB_NAMES = [
    (2000, "Candlelight"),
    (2700, "Incandescent"),
    (3200, "Warm white"),
    (4000, "Fluorescent"),
    (5500, "Daylight"),
    (6500, "Overcast"),
    (7500, "Shade"),
    (8000, "Deep shade"),
]


# ── Pure display helpers ──────────────────────────────────────────────────────

def wb_preset_name(k: int) -> str:
    if k < 2500: return "Incandescent"
    if k < 3500: return "Warm fluorescent"
    if k < 4500: return "Fluorescent"
    if k < 6000: return "Daylight"
    if k < 7000: return "Cloudy daylight"
    return "Shade"


def wb_name(k: int) -> str:
    return min(WB_NAMES, key=lambda x: abs(x[0] - k))[1]


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
        self.v_min = v_min
        self.v_max = v_max
        self.display_fn = display_fn or str
        self._spin_scale = spinbox_scale
        self._debounce: Optional[QTimer] = None

        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(0, self.STEPS)
        self._slider.setValue(0)
        self._slider.setMinimumWidth(140)
        lay.addWidget(self._slider, 1)

        self._val_lbl = QLabel(display_fn(v_min) if display_fn else str(v_min))
        self._val_lbl.setObjectName("val")
        self._val_lbl.setMinimumWidth(70)
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
        spin.setFixedWidth(100)
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
        if self._debounce:
            self._debounce.stop()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(lambda: self.value_changed.emit(val))
        self._debounce.start(80)

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

class WbSliderRow(QWidget):
    """Linear Kelvin slider 2000-8000 with spinbox for direct entry."""
    value_changed = pyqtSignal(int)

    def __init__(self, parent=None):
        super().__init__(parent)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(2000, 8000)
        self._slider.setValue(5500)
        self._slider.setMinimumWidth(140)
        self._slider.setSingleStep(50)
        self._slider.setPageStep(500)
        lay.addWidget(self._slider, 1)

        self._lbl = QLabel("Daylight")
        self._lbl.setObjectName("val")
        self._lbl.setMinimumWidth(70)
        lay.addWidget(self._lbl)

        self._spin = NoScrollSpinBox()
        self._spin.setRange(2000, 8000)
        self._spin.setValue(5500)
        self._spin.setSingleStep(50)
        self._spin.setSuffix(" K")
        self._spin.setFixedWidth(100)
        lay.addWidget(self._spin)

        self._debounce: Optional[QTimer] = None
        self._slider.valueChanged.connect(self._on_slider)
        self._spin.editingFinished.connect(self._on_spin)

    def _on_slider(self, k: int):
        self._lbl.setText(wb_name(k))
        self._spin.blockSignals(True)
        self._spin.setValue(k)
        self._spin.blockSignals(False)
        self._schedule_emit(k)

    def _on_spin(self):
        k = self._spin.value()
        self._slider.blockSignals(True)
        self._slider.setValue(k)
        self._slider.blockSignals(False)
        self._lbl.setText(wb_name(k))
        self._schedule_emit(k)

    def _schedule_emit(self, k: int):
        if self._debounce: self._debounce.stop()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(lambda: self.value_changed.emit(k))
        self._debounce.start(80)

    def get_value(self) -> int: return self._slider.value()

    def set_value(self, k: int):
        self._slider.blockSignals(True)
        self._slider.setValue(k)
        self._slider.blockSignals(False)
        self._spin.blockSignals(True)
        self._spin.setValue(k)
        self._spin.blockSignals(False)
        self._lbl.setText(wb_name(k))

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
        self._spin.setEnabled(enabled)


# ── Pan slider row ────────────────────────────────────────────────────────────

class PanSliderRow(QWidget):
    """Linear slider -1.0 to 1.0 with a centered zero tick."""
    value_changed = pyqtSignal(float)
    STEPS = 200

    def __init__(self, label_neg: str = "L", label_pos: str = "R", parent=None):
        super().__init__(parent)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        neg_lbl = QLabel(label_neg)
        neg_lbl.setObjectName("dim")
        lay.addWidget(neg_lbl)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(-self.STEPS, self.STEPS)
        self._slider.setValue(0)
        self._slider.setMinimumWidth(120)
        lay.addWidget(self._slider, 1)

        pos_lbl = QLabel(label_pos)
        pos_lbl.setObjectName("dim")
        lay.addWidget(pos_lbl)

        self._debounce: Optional[QTimer] = None
        self._slider.valueChanged.connect(self._on_slider)

    def _on_slider(self, pos: int):
        val = pos / self.STEPS
        if self._debounce:
            self._debounce.stop()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(lambda: self.value_changed.emit(val))
        self._debounce.start(30)

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
