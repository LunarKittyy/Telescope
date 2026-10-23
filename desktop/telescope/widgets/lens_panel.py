import re

from PyQt6.QtCore import QPointF, QSize, Qt, pyqtSignal
from PyQt6.QtGui import QColor, QFont, QFontMetrics, QPainter
from PyQt6.QtWidgets import QLabel, QPushButton, QSizePolicy, QStyle, QStyleOptionButton, QVBoxLayout, QWidget

from telescope import theme
from telescope.widgets.common import FlowLayout, cap_baseline, card_hue, stacked_baselines, ui_px
from telescope.i18n import tr

TILE_HEIGHT = 54
"""Height of a lens tile, the same whatever its label says."""

_FOCAL = re.compile(r"~?(\d+(?:\.\d+)?)\s*(mm|x)\b")


def split_lens_label(label: str) -> tuple:
    """(number, unit, the rest) of a shortened lens label: "Auto ~24mm OIS" is ("24", "mm", "Auto · OIS")."""
    match = _FOCAL.search(label)
    if not match:
        return label, "", ""
    rest = (label[:match.start()] + " " + label[match.end():]).replace("F/", "Front ")
    return match.group(1), match.group(2), " · ".join(rest.split())


class _LensButton(QPushButton):
    """One lens as a tile: its focal length big on top, what else sets it apart small below. The button's text stays
    the whole label, for the shortcuts menu and the tooltip."""

    def __init__(self, label: str):
        super().__init__(label)
        self._label = label
        self._number, self._unit, self._rest = split_lens_label(label)
        self.setToolTip(label)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)

    def label(self) -> str:
        return self._label

    def sizeHint(self) -> QSize:
        return QSize(ui_px(64), ui_px(TILE_HEIGHT))

    def minimumSizeHint(self) -> QSize:
        return QSize(1, ui_px(TILE_HEIGHT))

    def paintEvent(self, _event):
        p = QPainter(self)
        opt = QStyleOptionButton()
        self.initStyleOption(opt)
        opt.text = ""
        self.style().drawControl(QStyle.ControlElement.CE_PushButton, opt, p, self)
        on = self.isChecked()
        ink = QColor(card_hue(self) if on else theme.TEXT) if self.isEnabled() else QColor(theme.TEXT_DISABLED)
        big, small = QFont(self.font()), QFont(self.font())
        big.setPointSizeF(self.font().pointSizeF() * 1.25)
        big.setWeight(QFont.Weight.DemiBold)
        small.setPointSizeF(self.font().pointSizeF() * 0.85)
        first, second = stacked_baselines(QFontMetrics(big), QFontMetrics(small), 0, self.height(), 8)
        if not self._rest:
            first = cap_baseline(QFontMetrics(big), 0, self.height())  # a lone number sits in the middle
        width = self.width() - 8
        p.setPen(ink)
        p.setFont(big)
        number = p.fontMetrics().elidedText(self._number, Qt.TextElideMode.ElideRight, width)
        n_w = p.fontMetrics().horizontalAdvance(number)
        unit = QFont(small)
        unit.setWeight(QFont.Weight.Medium)
        u_w = QFontMetrics(unit).horizontalAdvance(self._unit) if number == self._number else 0
        x = 4 + max(0, (width - n_w - u_w) // 2)
        p.drawText(QPointF(x, first), number)
        if u_w:
            p.setFont(unit)
            p.drawText(QPointF(x + n_w + 1, first), self._unit)
        if self._rest:
            p.setFont(small)
            faint = QColor(ink if on else theme.TEXT_FAINT)
            p.setPen(faint)
            rest = p.fontMetrics().elidedText(self._rest, Qt.TextElideMode.ElideRight, width)
            p.drawText(QPointF(4 + (width - p.fontMetrics().horizontalAdvance(rest)) / 2, second), rest)
        p.end()


def _columns(count: int) -> int:
    """Up to five tiles in a row; more lenses split into rows as even as they can be."""
    if count == 0:
        return 1
    rows = -(-count // 5)
    return max(1, -(-count // rows))


def shorten_lens_label(raw: str, compact: bool = True) -> str:
    """Strip Android boilerplate ("Back", "[phys]") from camera names; "[auto]" marks a multi-lens camera.

    "Auto" goes first: the pill elides from the right, and it's what tells the multi-lens camera apart
    from the physical lens it's named after ("~22mm OIS" and "~22mm OIS Auto" both cut to "~22mm OIS...").
    """
    auto = " [auto]" in raw
    label = (raw.replace(" [phys]", "")
               .replace(" [auto]", "")
               .replace("Back ", "")
               .replace("Front ", "F/")
               .strip())
    if compact:
        label = label.replace("Telephoto", "Tele")
    return f"Auto {label}" if auto else label


class LensPanel(QWidget):
    lens_selected = pyqtSignal(dict)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("lens_panel")
        self.setSizePolicy(QSizePolicy.Policy.Preferred, QSizePolicy.Policy.Minimum)

        # The placeholder sits outside the flow layout: it's a full-width
        # message, not one of the wrapping pills.
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)

        self._ph = QLabel(tr("Start streaming to load lenses"))
        self._ph.setObjectName("dim")
        outer.addWidget(self._ph)

        self._flow_host = QWidget()
        self._flow_host.setObjectName("lens_panel")
        # Equal tiles in fixed columns, so they never reflow into odd sizes whatever the labels say.
        self._layout = FlowLayout(self._flow_host, spacing=6, uniform=True, columns=1)
        outer.addWidget(self._flow_host)
        self._flow_host.hide()

        self._cameras: list = []
        self._btns:    list = []

    def load(self, cameras: list):
        if not cameras:
            self.clear()
            self.set_placeholder(tr("No cameras available"))
            return
        self._ph.hide()
        self._flow_host.show()
        for b in self._btns:
            self._layout.removeWidget(b)
            b.deleteLater()
        self._btns.clear()
        self._cameras = cameras

        self._layout._columns = _columns(len(cameras))
        for cam in cameras:
            btn = _LensButton(shorten_lens_label(cam["label"]))
            btn.setProperty("shortcutValue", shorten_lens_label(cam["label"], compact=False))
            btn.setObjectName("lens_button")
            btn.setCheckable(True)
            btn.setChecked(cam.get("current", False))
            btn.clicked.connect(lambda _, c=cam, b=btn: self._select(c, b))
            self._layout.addWidget(btn)
            self._btns.append(btn)
        self._flow_host.updateGeometry()

    def _select(self, cam: dict, clicked_btn: QPushButton):
        for b in self._btns: b.setChecked(False)
        clicked_btn.setChecked(True)
        self.lens_selected.emit(cam)

    def select_id(self, cam_id: str):
        """Check the lens with this id without emitting; returns its dict, or None if this phone has none."""
        for cam, btn in zip(self._cameras, self._btns):
            if cam.get("id") == cam_id:
                for b in self._btns:
                    b.setChecked(b is btn)
                return cam
        return None

    def buttons(self) -> list:
        """One per lens, in order; checked is the one in use."""
        return list(self._btns)

    def set_placeholder(self, text: str):
        self._ph.setText(text)
        if not self._btns:
            self._flow_host.hide()
            self._ph.show()

    def clear(self):
        for b in self._btns:
            self._layout.removeWidget(b)
            b.deleteLater()
        self._btns.clear()
        self._cameras = []
        self._flow_host.hide()
        self._ph.setText(tr("Start streaming to load lenses"))
        self._ph.show()
