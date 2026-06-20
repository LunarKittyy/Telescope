from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import QGridLayout, QLabel, QPushButton, QWidget


class LensPanel(QWidget):
    lens_selected = pyqtSignal(dict)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._layout  = QGridLayout(self)
        self._layout.setContentsMargins(0, 0, 0, 0)
        self._layout.setSpacing(4)
        self._cameras: list = []
        self._btns:    list = []
        self._ph = QLabel("Start streaming to load lenses")
        self._ph.setObjectName("dim")
        self._layout.addWidget(self._ph, 0, 0)

    def load(self, cameras: list):
        self._ph.hide()
        for b in self._btns: b.deleteLater()
        self._btns.clear()
        self._cameras = cameras
        cols = 3
        for i, cam in enumerate(cameras):
            lbl = cam["label"].replace(" [phys]", "").replace("Back ", "").replace("Front ", "F/")
            btn = QPushButton(lbl)
            btn.setCheckable(True)
            btn.setChecked(cam.get("current", False))
            btn.clicked.connect(lambda _, c=cam, b=btn: self._select(c, b))
            self._layout.addWidget(btn, i // cols, i % cols)
            self._btns.append(btn)

    def _select(self, cam: dict, clicked_btn: QPushButton):
        for b in self._btns: b.setChecked(False)
        clicked_btn.setChecked(True)
        self.lens_selected.emit(cam)

    def set_placeholder(self, text: str):
        self._ph.setText(text)
        if not self._btns:
            self._ph.show()

    def clear(self):
        for b in self._btns: b.deleteLater()
        self._btns.clear()
        self._cameras.clear()
        self._ph.setText("Start streaming to load lenses")
        self._ph.show()
