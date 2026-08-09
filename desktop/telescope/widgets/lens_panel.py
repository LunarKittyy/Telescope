from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtGui import QFontMetrics
from PyQt6.QtWidgets import QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget

from telescope.widgets.common import FlowLayout

# Max label width before eliding; full text goes to tooltip.
_MAX_LABEL_W = 122


def shorten_lens_label(raw: str) -> str:
    """Strip Android boilerplate ("Back", "[phys]") from camera names."""
    return (raw.replace(" [phys]", "")
               .replace("Back ", "")
               .replace("Front ", "F/")
               .strip())


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
        self._layout = FlowLayout(self._flow_host, spacing=5, uniform=True)
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
            btn = QPushButton()
            btn.setObjectName("lens_button")
            btn.setMinimumHeight(30)
            btn.setCheckable(True)
            btn.setChecked(cam.get("current", False))
            metrics = QFontMetrics(btn.font())
            btn.setText(metrics.elidedText(label, Qt.TextElideMode.ElideRight, _MAX_LABEL_W))
            if btn.text() != label:
                btn.setToolTip(label)
            btn.clicked.connect(lambda _, c=cam, b=btn: self._select(c, b))
            self._layout.addWidget(btn)
            self._btns.append(btn)
        self._flow_host.updateGeometry()

    def _select(self, cam: dict, clicked_btn: QPushButton):
        for b in self._btns: b.setChecked(False)
        clicked_btn.setChecked(True)
        self.lens_selected.emit(cam)

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
        self._cameras.clear()
        self._flow_host.hide()
        self._ph.setText("Start streaming to load lenses")
        self._ph.show()
