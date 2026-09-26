import math
import threading

from PyQt6.QtCore import QByteArray, QCoreApplication, QEventLoop, QMetaObject, QThread, QPoint, QRect, QRectF, QSize, Qt, pyqtSignal
from PyQt6.QtSvg import QSvgRenderer
from PyQt6.QtGui import (
    QBrush, QColor, QFontMetrics, QIcon, QPainter, QPainterPath, QPixmap,
)
from PyQt6.QtWidgets import (
    QApplication, QComboBox, QDoubleSpinBox, QFrame, QHBoxLayout, QLabel, QLayout, QPushButton,
    QSlider, QSpinBox, QSizePolicy, QStyle, QStyleOptionSlider, QVBoxLayout, QWidget,
)

from telescope import theme


# ── Shared desktop UI primitives ─────────────────────────────────────────────
# Layout rules every card follows (keep new controls inside them):
#   - Type: one family. 14pt dialog titles, 11pt card titles, 9.5pt everything else, 7.5pt caps
#     section headings.
#   - Row: [label column, top-anchored][control column]; rows are at least ROW_HEIGHT tall.
#   - Text, number and select inputs fill the control column (control_row(..., stretch=True)).
#   - Segmented toggles (SegmentButton) fill the control column in equal shares, like the inputs
#     around them; outside a card they use SEGMENT_WIDTH per segment. Text fits the segment, not
#     the other way round. Checkboxes keep their natural width at the column start.
#   - Slider rows are slider_row(): [slider][value column]; in a card where any slider row has a
#     direct-entry spinbox, every slider row reserves that column (gutter=True) so tracks end together.
#   - Card-level actions (Pair, Reset) go in the card header, not in a row.
#   - On/off settings are checkboxes labelled "On".
#   - Buttons: one primary per view (the thing you came to do), "danger" only for destructive
#     actions, everything else default. A lone action button is BUTTON_WIDTH wide; buttons sharing
#     a row split it equally (button_row). Dialogs: dialog_header() on top, sections are cards,
#     dialog_buttons() bottom-right.

#   - Every width and height constant below is in design pixels, measured against the Inter UI
#     font. Pass them through ui_px(), which scales them up when the font the user actually gets
#     is wider (another fallback font, larger system text, a different DPI), so text keeps fitting.

FORM_LABEL_WIDTH = 112

SEGMENT_WIDTH = 78
"""Width of one segment where a toggle has no control column to fill (the header)."""

BUTTON_WIDTH = 160
"""Width of a lone action button; its text has to fit, not the other way round."""

DIALOG_BUTTON_WIDTH = 96
"""Width of the Close / OK / Cancel buttons in a dialog's bottom bar."""

ROW_HEIGHT = 32
"""Minimum height of a settings row; matches the input controls."""

VALUE_COL_WIDTH = 56
"""Width of numeric readout (fixed, right-aligned so readouts line up)."""

SPIN_COL_WIDTH = 78
"""Width of the direct-entry spinbox beside a slider."""

SPIN_COL_GUTTER = SPIN_COL_WIDTH + 8
"""Space slider rows reserve for alignment with spinbox rows."""

SLIDER_TRACK_WIDTH = 84
"""Minimum width for slider track (floor for draggability; apply via stretch_slider())."""

_SCALE_PROBE = "Apply and reload"
_SCALE_PROBE_PX = 125  # its advance in the button font at the design size (Inter 9.5pt, 96 dpi)
_scale_cache: dict = {}


def _probe_advance() -> float:
    probe = QPushButton(_SCALE_PROBE)
    probe.ensurePolished()
    return probe.fontMetrics().horizontalAdvance(_SCALE_PROBE)


def ui_scale() -> float:
    """How much wider the real UI font is than the design font; never below 1."""
    app = QApplication.instance()
    if app is None:
        return 1.0
    screen = app.primaryScreen()
    key = (len(app.styleSheet()), app.font().key(), screen.logicalDotsPerInch() if screen else 96)
    if key not in _scale_cache:
        _scale_cache[key] = max(1.0, _probe_advance() / _SCALE_PROBE_PX)
    return _scale_cache[key]


def ui_px(design_px: int) -> int:
    """A design-pixel size scaled to the font actually in use (see ui_scale())."""
    return round(design_px * ui_scale())


def stretch_slider(slider: QWidget, minimum: int = SLIDER_TRACK_WIDTH) -> QWidget:
    """Let a slider grow with its column while keeping a draggable minimum."""
    slider.setMinimumWidth(ui_px(minimum))
    slider.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
    return slider


class ElidingLabel(QLabel):
    """Label that elides text instead of forcing row wider (full text in tooltip)."""

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
    """Wrapping flow layout that sizes items to content and adapts column count to available space."""

    def __init__(self, parent=None, spacing: int = 6, uniform: bool = False, columns: int = 0):
        super().__init__(parent)
        self._items: list = []
        self._spacing = spacing
        # uniform: equal-width items, rows end flush (grid-like, not pill pile).
        self._uniform = uniform
        # columns > 0 fixes the grid instead of deriving it from the widest item's text.
        self._columns = columns
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
        """Place items and return total height (measurement pass when apply=False)."""
        if not self._items:
            return 0

        m = self.contentsMargins()
        left = rect.x() + m.left()
        y = rect.y() + m.top()
        avail = max(rect.width() - m.left() - m.right(), 1)

        if self._uniform:
            widest = max(i.sizeHint().width() for i in self._items)
            height = max(i.sizeHint().height() for i in self._items)
            if self._columns:
                per_row = self._columns
            else:
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


def set_status_kind(label: QWidget, kind: str):
    """Switch a label between the status_* QSS roles; a bare setObjectName() doesn't re-apply the stylesheet."""
    label.setObjectName(kind)
    style = label.style()
    style.unpolish(label)
    style.polish(label)


def make_segmented(*buttons: QWidget):
    """Style button run as segmented pill strip (purely presentational; signals untouched)."""
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


class SegmentButton(QPushButton):
    """One checkable segment of a segmented toggle; exclusivity comes from the QButtonGroup it's added to."""

    def __init__(self, text: str = "", parent=None):
        super().__init__(text, parent)
        self.setCheckable(True)
        self.setAutoExclusive(False)


def segmented_row(*buttons: QWidget, fill: bool = True) -> QHBoxLayout:
    """Zero-gap run of segments, all the same width: equal shares of the row when fill, else SEGMENT_WIDTH each."""
    make_segmented(*buttons)
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(0)
    for btn in buttons:
        if fill:
            btn.setMinimumWidth(1)
            btn.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
            lay.addWidget(btn, 1)
        else:
            btn.setFixedWidth(ui_px(SEGMENT_WIDTH))
            lay.addWidget(btn)
    return lay


def create_card(parent=None) -> QFrame:
    card = QFrame(parent)
    card.setObjectName("card")
    card.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred)
    return card


def card_layout(card: QFrame) -> QVBoxLayout:
    """The standard padded column every main-window card uses."""
    lay = QVBoxLayout(card)
    lay.setContentsMargins(18, 16, 18, 18)
    lay.setSpacing(8)
    return lay


def add_card_header(layout: QVBoxLayout, title: str, icon_name: str,
                    subtitle: str = "", action: QWidget = None) -> QHBoxLayout:
    """Add the standard icon/title header used by every main-window card; action is the card's one button, top right."""
    header = QHBoxLayout()
    header.setContentsMargins(0, 0, 0, 2)
    header.setSpacing(10)

    if icon_name:
        icon = QLabel()
        icon.setPixmap(create_vector_icon(icon_name, theme.ACCENT).pixmap(20, 20))
        icon.setFixedSize(20, 20)
        header.addWidget(icon)

    title_label = QLabel(title)
    title_label.setObjectName("card_title")
    # Fixed height whether or not the card has a header action, so titles line up across cards.
    title_label.setFixedHeight(30)
    header.addWidget(title_label)

    if subtitle:
        subtitle_label = QLabel(subtitle)
        subtitle_label.setObjectName("card_subtitle")
        header.addWidget(subtitle_label)

    header.addStretch()
    if action is not None:
        header.addWidget(action)
    layout.addLayout(header)
    return header


def action_button(text: str, role: str = "", tooltip: str = "") -> QPushButton:
    """A lone action button at the standard width; role is "", "primary" or "danger"."""
    btn = QPushButton(text)
    btn.setFixedWidth(ui_px(BUTTON_WIDTH))
    if role:
        set_ui_role(btn, role)
    if tooltip:
        btn.setToolTip(tooltip)
    return btn


def button_row(*buttons: QPushButton) -> QHBoxLayout:
    """Buttons that share a row split it into equal widths."""
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    for btn in buttons:
        btn.setMinimumWidth(1)
        btn.setMaximumWidth(16777215)
        btn.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        lay.addWidget(btn, 1)
    return lay


def dialog_layout(dialog: QWidget) -> QVBoxLayout:
    lay = QVBoxLayout(dialog)
    lay.setContentsMargins(20, 18, 20, 18)
    lay.setSpacing(14)
    return lay


def dialog_header(layout: QVBoxLayout, title: str, subtitle: str = "") -> QLabel:
    """Title plus one line of context; returns the subtitle label so callers can update it."""
    box = QVBoxLayout()
    box.setContentsMargins(0, 0, 0, 0)
    box.setSpacing(2)
    title_lbl = QLabel(title)
    title_lbl.setObjectName("dialog_title")
    box.addWidget(title_lbl)
    sub = WrapLabel(subtitle)
    sub.setObjectName("dialog_subtitle")
    sub.setVisible(bool(subtitle))
    box.addWidget(sub)
    layout.addLayout(box)
    return sub


def dialog_buttons(layout: QVBoxLayout, *buttons: QPushButton) -> QHBoxLayout:
    """Bottom-right button bar; pass the primary last so it sits at the corner."""
    bar = QHBoxLayout()
    bar.setContentsMargins(0, 4, 0, 0)
    bar.setSpacing(8)
    bar.addStretch(1)
    for btn in buttons:
        btn.setFixedWidth(ui_px(DIALOG_BUTTON_WIDTH))
        bar.addWidget(btn)
    layout.addLayout(bar)
    return bar


class WrapLabel(QLabel):
    """Word-wrapped label that reserves the height its current width needs.

    Plain wrapped QLabels report a one-line minimum to box layouts, so dialogs sized before the text
    wrapped clipped it. Re-pinning the minimum on every resize makes the layout (and the window) grow.
    """

    def __init__(self, text: str = "", parent=None):
        super().__init__(text, parent)
        self.setWordWrap(True)

    def _fit(self):
        # Measured from the text itself: heightForWidth() never reports less than the pinned minimum,
        # so a label pinned while narrow would keep that height after widening.
        width = self.contentsRect().width()
        if width <= 1:
            return
        m = self.contentsMargins()
        need = self.fontMetrics().boundingRect(
            0, 0, width, 100_000, int(Qt.TextFlag.TextWordWrap | self.alignment().value), self.text(),
        ).height() + m.top() + m.bottom()
        if need != self.minimumHeight():
            self.setMinimumHeight(need)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._fit()

    def setText(self, text: str):
        super().setText(text)
        self._fit()


def wrapped_note(text: str, kind: str = "dim") -> QLabel:
    """Explanatory text that sits in a card's control column."""
    lbl = WrapLabel(text)
    lbl.setObjectName(kind)
    return lbl


def card_action(text: str, icon_name: str, tooltip: str = "") -> QPushButton:
    """The small quiet button a card header carries."""
    btn = QPushButton(text)
    btn.setObjectName("card_action")
    btn.setIcon(create_vector_icon(icon_name, theme.TEXT_DIM))
    btn.setIconSize(QSize(15, 15))
    if tooltip:
        btn.setToolTip(tooltip)
    return btn


def value_label(text: str = "") -> QLabel:
    """Fixed-width, right-aligned readout that sits after a slider."""
    lbl = QLabel(text)
    lbl.setObjectName("val")
    lbl.setFixedWidth(ui_px(VALUE_COL_WIDTH))
    lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
    return lbl


def slider_row(slider: QWidget, readout: QLabel, gutter: bool = False) -> QHBoxLayout:
    """[slider][value column], plus the spinbox column's width when the card has direct-entry rows."""
    stretch_slider(slider)
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    lay.addWidget(slider, 1)
    lay.addWidget(readout)
    if gutter:
        lay.addSpacing(ui_px(SPIN_COL_GUTTER))
    return lay


def add_section_heading(layout: QVBoxLayout, text: str):
    """Quiet small-caps divider between groups of rows; spacing, not a rule, does the separating."""
    heading = QLabel(text.upper())
    heading.setIndent(0)  # QSS margins switch on QLabel's auto-indent, which pushed headings ~4px right of the row labels
    heading.setObjectName("section_title")
    layout.addWidget(heading)
    return heading


def form_label(text: str, width: int = FORM_LABEL_WIDTH) -> QLabel:
    label = QLabel(text)
    label.setObjectName("form_label")
    label.setFixedWidth(ui_px(width))
    label.setMinimumHeight(ui_px(ROW_HEIGHT))  # every row, text-only or not, keeps the same rhythm
    label.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
    return label


def control_row(label: str, widget, label_width: int = FORM_LABEL_WIDTH,
                stretch: bool = False) -> QHBoxLayout:
    """Settings row: dim label, then the control starting at a shared column so label and value sit side by side."""
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(10)
    # Top-anchored: when the control wraps (lens pills) the label lines up with its first line, not the block's middle.
    lay.addWidget(form_label(label, label_width), 0, Qt.AlignmentFlag.AlignTop)
    if isinstance(widget, QLayout): lay.addLayout(widget, 1 if stretch else 0)
    else:                           lay.addWidget(widget, 1 if stretch else 0)
    if not stretch:
        lay.addStretch(1)
    return lay


def control_row_widget(label: str, widget, label_width: int = FORM_LABEL_WIDTH,
                       stretch: bool = False) -> QWidget:
    """A hideable `control_row` - for rows revealed by a mode toggle."""
    container = QWidget()
    container.setObjectName("form_row")
    container.setLayout(control_row(label, widget, label_width, stretch))
    return container


def run_off_ui_thread(fn, *args, **kwargs):
    """Run a blocking call (adb, mostly) on a worker thread and return its result, repainting meanwhile.

    User input is held back until it returns, so nothing can re-enter the caller mid-call. Off the
    GUI thread, or with no QApplication, it just calls fn.
    """
    app = QCoreApplication.instance()
    if app is None or QThread.currentThread() is not app.thread():
        return fn(*args, **kwargs)
    loop = QEventLoop()
    done = threading.Event()
    outcome = {}

    def work():
        try:
            outcome["value"] = fn(*args, **kwargs)
        except BaseException as exc:
            outcome["error"] = exc
        finally:
            done.set()
            # Queued: if it lands before exec() starts it's still delivered by exec(), so no lost wakeup.
            QMetaObject.invokeMethod(loop, "quit", Qt.ConnectionType.QueuedConnection)

    threading.Thread(target=work, daemon=True).start()
    if not done.is_set():
        loop.exec(QEventLoop.ProcessEventsFlag.ExcludeUserInputEvents)
    if "error" in outcome:
        raise outcome["error"]
    return outcome.get("value")


# ── Pure display helpers ──────────────────────────────────────────────────────


def ns_to_display(ns: int) -> str:
    if ns <= 0: return "?"
    s = ns / 1_000_000_000.0
    if s >= 1.0:
        return f"{s:.1f} s"
    denom = round(1.0 / s)
    return f"1/{denom:,}"


def quality_label(q: int) -> str:
    if q >= 95: return f"{q}%: High"
    if q >= 80: return f"{q}%: Balanced"
    if q >= 60: return f"{q}%: Low"
    return f"{q}%: Very low"


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

    def showPopup(self):
        # Qt sizes/positions the popup before it can know it overshoots the screen -
        # a long list flipped upward routinely pokes above the top edge. Clamp after the fact.
        super().showPopup()
        popup = self.view().window()
        screen = self.screen()
        if popup is None or screen is None:
            return
        avail = screen.availableGeometry()
        margin = 8
        top = max(popup.y(), avail.y() + margin)
        bottom = min(popup.y() + popup.height(), avail.y() + avail.height() - margin)
        if top != popup.y() or bottom != popup.y() + popup.height():
            popup.setGeometry(popup.x(), top, popup.width(), max(bottom - top, 50))


class NoScrollSlider(QSlider):
    def wheelEvent(self, event):
        event.ignore()


class ZoomSlider(NoScrollSlider):
    """The Zoom slider, with a dot on the groove at each value in set_marks (where a longer lens takes over)."""

    _MARK = 5  # dot diameter, px; a bit wider than the 4px groove so it shows on the filled part too

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._marks: list = []

    def set_marks(self, values):
        self._marks = sorted(values)
        self.update()

    def marks(self) -> list:
        return list(self._marks)

    def mark_x(self, value: int) -> float:
        """Where the handle's centre sits at `value`, in widget px (asked of the style, margins and all)."""
        def centre(pos):
            opt = QStyleOptionSlider()
            self.initStyleOption(opt)
            opt.sliderPosition = opt.sliderValue = pos
            return QRectF(self.style().subControlRect(QStyle.ComplexControl.CC_Slider, opt,
                                                      QStyle.SubControl.SC_SliderHandle, self)).center().x()
        lo, hi = self.minimum(), self.maximum()
        if hi <= lo:
            return centre(lo)
        a, b = centre(lo), centre(hi)
        return a + (b - a) * (value - lo) / (hi - lo)

    def paintEvent(self, event):
        super().paintEvent(event)
        marks = [m for m in self._marks if self.minimum() < m <= self.maximum()]
        if not marks:
            return
        opt = QStyleOptionSlider()
        self.initStyleOption(opt)
        handle = QRectF(self.style().subControlRect(QStyle.ComplexControl.CC_Slider, opt,
                                                    QStyle.SubControl.SC_SliderHandle, self))
        groove = QRectF(self.style().subControlRect(QStyle.ComplexControl.CC_Slider, opt,
                                                    QStyle.SubControl.SC_SliderGroove, self))
        # On top of the fill, but cut around the (round) handle so a mark never sits over it.
        clip = QPainterPath()
        clip.addRect(QRectF(self.rect()))
        knob = QPainterPath()
        knob.addEllipse(handle)
        p = QPainter(self)
        p.setClipPath(clip.subtracted(knob))
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setPen(Qt.PenStyle.NoPen)
        d = ui_px(self._MARK)
        y = groove.center().y()
        on_groove = QColor(theme.TEXT_DIM if self.isEnabled() else theme.TEXT_DISABLED)
        on_fill = QColor(theme.TEXT)  # lighter on the lavender part left of the handle
        for m in marks:
            p.setBrush(on_fill if m < self.value() else on_groove)
            p.drawEllipse(QRectF(self.mark_x(m) - d / 2, y - d / 2, d, d))


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


def create_app_icon(size: int = 32) -> QIcon:
    """Telescope mark: accent-blue disc with dark centre punched out; only app icon asset, used everywhere for consistency."""
    pixmap = QPixmap(size, size)
    pixmap.fill(Qt.GlobalColor.transparent)

    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    painter.setPen(Qt.PenStyle.NoPen)

    margin = size / 22
    outer_d = size - 2 * margin
    painter.setBrush(QBrush(QColor(theme.ACCENT)))
    painter.drawEllipse(QRectF(margin, margin, outer_d, outer_d))

    inner_margin = size * 7 / 22
    inner_d = size * 8 / 22
    painter.setBrush(QBrush(QColor(theme.BG)))
    painter.drawEllipse(QRectF(inner_margin, inner_margin, inner_d, inner_d))

    painter.end()
    return QIcon(pixmap)


# Icon artwork: 24-unit grid, rounded 2.2 strokes, and a soft 25% fill on each
# icon's main shape so the set has some body at 16-20 px. "{c}" is the colour.
_SOFT = 'fill="{c}" fill-opacity="0.25"'
_ICON_SVG = {
    "connection": '''<path d="M10 14a4.2 4.2 0 0 0 6 0l3-3a4.2 4.2 0 0 0-6-6l-1.2 1.2"/>
        <path d="M14 10a4.2 4.2 0 0 0-6 0l-3 3a4.2 4.2 0 0 0 6 6l1.2-1.2"/>''',
    "usb": f'''<rect x="8" y="2.5" width="8" height="6" rx="1.5" {_SOFT}/>
        <path d="M6.5 8.5h11v4.5a5.5 5.5 0 0 1-11 0z"/><path d="M12 18.5v3"/>''',
    "camera": f'''<path d="M4.5 7.5h2.8l1.8-2.6h5.8l1.8 2.6h2.8a1.5 1.5 0 0 1 1.5 1.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18V9a1.5 1.5 0 0 1 1.5-1.5z" {_SOFT}/>
        <circle cx="12" cy="13.2" r="3.4"/>''',
    "stream": f'''<rect x="3" y="4" width="18" height="12.5" rx="2.2" {_SOFT}/>
        <path d="M8.5 20.5h7M12 16.5v4"/>''',
    "gear": f'''<path d="M4 7h9M19 7h1M4 17h3M13 17h7"/>
        <circle cx="16" cy="7" r="2.6" {_SOFT}/><circle cx="10" cy="17" r="2.6" {_SOFT}/>''',
    "status": f'''<rect x="3" y="3" width="18" height="18" rx="4.5" {_SOFT} stroke="none"/>
        <path d="M5.5 12.5h3l2-4.5 3 8.5 2-4h3"/>''',
    "qr": f'''<rect x="3.5" y="3.5" width="7" height="7" rx="1.6" {_SOFT}/>
        <rect x="13.5" y="3.5" width="7" height="7" rx="1.6" {_SOFT}/>
        <rect x="3.5" y="13.5" width="7" height="7" rx="1.6" {_SOFT}/>
        <path d="M14 14h2.5v2.5M20.5 14v.01M14 20.5h6.5v-3.5"/>''',
    "play": '''<path d="M8 5.8v12.4a1.2 1.2 0 0 0 1.8 1l9.6-6.2a1.2 1.2 0 0 0 0-2l-9.6-6.2A1.2 1.2 0 0 0 8 5.8z" fill="{c}"/>''',
    "stop": '''<rect x="6" y="6" width="12" height="12" rx="2.6" fill="{c}"/>''',
    "expand": f'''<rect x="3" y="7" width="14" height="14" rx="2.2" {_SOFT}/>
        <path d="M13.5 3h7.5v7.5M21 3l-8.5 8.5"/>''',
    "lenses": f'''<rect x="3" y="5" width="18" height="14" rx="2.2" {_SOFT}/>
        <rect x="8.5" y="9" width="7" height="6" rx="1.2"/>''',
    "reset": '''<path d="M3.5 12a8.5 8.5 0 1 0 2.6-6.1L3.5 8.5"/><path d="M3.5 3.5v5h5"/>''',
    "transforms": f'''<path d="M12 3v2.5M12 9.5v5M12 18.5V21"/>
        <path d="M9 6.5L3.5 17.5H9z" {_SOFT}/><path d="M15 6.5l5.5 11H15z"/>''',
    "check": f'''<circle cx="12" cy="12" r="8.8" {_SOFT}/><path d="M8 12.3l2.8 2.8 5.2-5.6"/>''',
    "update": f'''<circle cx="12" cy="12" r="8.8" {_SOFT}/><path d="M12 7.5v8.5M8.3 12.6l3.7 3.7 3.7-3.7"/>''',
    "devices": f'''<rect x="3.5" y="4" width="10" height="17" rx="2.2" {_SOFT}/>
        <path d="M7.5 17.5h2"/><path d="M16.5 7.5h2.5a1.5 1.5 0 0 1 1.5 1.5v9.5a1.5 1.5 0 0 1-1.5 1.5h-2.5"/>''',
    "alert": f'''<path d="M10.3 4.2a2 2 0 0 1 3.4 0l7.4 12.8a2 2 0 0 1-1.7 3H4.6a2 2 0 0 1-1.7-3z" {_SOFT}/>
        <path d="M12 9.5v4"/><path d="M12 16.8v.2"/>''',
    "preset": f'''<path d="M6.5 3.5h11a1 1 0 0 1 1 1v16l-6.5-4.2-6.5 4.2v-16a1 1 0 0 1 1-1z" {_SOFT}/>''',
    "mic": f'''<rect x="8.5" y="3" width="7" height="11.5" rx="3.5" {_SOFT}/>
        <path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3"/>''',
    "close": '''<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>''',
}

_ICON_RENDER_PX = 64  # rendered once, large; QIcon scales down smoothly for every use


def create_vector_icon(icon_name: str, color_hex: str) -> QIcon:
    body = _ICON_SVG.get(icon_name, "").replace("{c}", color_hex)  # unknown name: blank icon, not a crash
    svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" '
        f'stroke="{color_hex}" stroke-width="2.2" stroke-linecap="round" '
        f'stroke-linejoin="round">{body}</svg>'
    )
    pixmap = QPixmap(_ICON_RENDER_PX, _ICON_RENDER_PX)
    pixmap.fill(Qt.GlobalColor.transparent)
    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    QSvgRenderer(QByteArray(svg.encode())).render(painter)
    painter.end()
    return QIcon(pixmap)


# ── Log-scale slider row ──────────────────────────────────────────────────────

class LogSliderRow(QWidget):
    """Horizontal log-scale slider with spinbox; spinbox_scale multiplies display (e.g., 1e-6 shows nanoseconds as milliseconds)."""
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
        self._val_lbl.setFixedWidth(ui_px(VALUE_COL_WIDTH))
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
        spin.setFixedWidth(ui_px(SPIN_COL_WIDTH))
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
        self.value_changed.emit(val)

    def _on_spin(self):
        val = float(self._spin.value()) / self._spin_scale
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))
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
            self.setMinimumWidth(ui_px(SLIDER_TRACK_WIDTH))

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
