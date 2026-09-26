from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtWidgets import QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget

from telescope.widgets.common import FlowLayout

_TEXT_PAD = 30  # the lens_button QSS padding + border, both sides


class _LensButton(QPushButton):
    """Lens pill whose label elides to the width the grid gives it, re-fitting on every resize."""

    def __init__(self, label: str):
        super().__init__()
        self._label = label

    def label(self) -> str:
        return self._label

    def resizeEvent(self, event):
        super().resizeEvent(event)
        fitted = self.fontMetrics().elidedText(
            self._label, Qt.TextElideMode.ElideRight, max(self.width() - _TEXT_PAD, 1))
        self.setText(fitted)
        self.setToolTip(self._label if fitted != self._label else "")


def shorten_lens_label(raw: str) -> str:
    """Strip Android boilerplate ("Back", "[phys]") from camera names; "[auto]" marks a multi-lens camera.

    "Auto" goes first: the pill elides from the right, and it's what tells the multi-lens camera apart
    from the physical lens it's named after ("~22mm OIS" and "~22mm OIS Auto" both cut to "~22mm OIS...").
    """
    auto = " [auto]" in raw
    label = (raw.replace(" [phys]", "")
               .replace(" [auto]", "")
               .replace("Back ", "")
               .replace("Front ", "F/")
               .replace("Telephoto", "Tele")  # keeps the zoom factor ("Tele 3x") inside a grid cell
               .strip())
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

        self._ph = QLabel("Start streaming to load lenses")
        self._ph.setObjectName("dim")
        outer.addWidget(self._ph)

        self._flow_host = QWidget()
        self._flow_host.setObjectName("lens_panel")
        # Two fixed columns: the grid comes from the layout, and long lens names elide to fit it.
        self._layout = FlowLayout(self._flow_host, spacing=6, uniform=True, columns=2)
        outer.addWidget(self._flow_host)
        self._flow_host.hide()

        self._cameras: list = []
        self._btns:    list = []

    def load(self, cameras: list):
        self._ph.hide()
        self._flow_host.show()
        for b in self._btns:
            self._layout.removeWidget(b)
            b.deleteLater()
        self._btns.clear()
        self._cameras = cameras

        for cam in cameras:
            label = shorten_lens_label(cam["label"])
            btn = _LensButton(label)
            btn.setObjectName("lens_button")
            btn.setMinimumHeight(30)
            btn.setMinimumWidth(1)
            btn.setCheckable(True)
            btn.setChecked(cam.get("current", False))
            btn.setText(label)
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
        self._ph.setText("Start streaming to load lenses")
        self._ph.show()
