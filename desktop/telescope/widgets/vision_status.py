"""The status row on cards that use vision: why nothing's happening yet, with a Download button when that's why."""
from PyQt6.QtWidgets import QHBoxLayout, QPushButton, QWidget

from telescope import vision
from telescope.widgets import vision_download
from telescope.widgets.common import wrapped_note


class VisionStatus(QWidget):
    """Hidden while there's nothing to say; refresh() after anything on the card that changes what it would say."""

    def __init__(self, host, bus, parent=None):
        super().__init__(parent)
        self._host = host
        self._active, self._problem = False, ""
        self._label = wrapped_note("", "status_warn")
        self._download_btn = QPushButton("Download")
        self._download_btn.clicked.connect(self._download)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        lay.addWidget(self._label, 1)
        lay.addWidget(self._download_btn)
        bus.vision_state_changed.connect(lambda _state: self._show())
        dl = vision_download.shared()
        dl.progress.connect(self._on_progress)
        dl.finished.connect(lambda _error: self._show())
        self._show()

    def text(self) -> str:
        return self._label.text()

    def refresh(self, active: bool, problem: str = ""):
        """active: the card is switched to something that needs vision; problem: the card's own reason, shown first."""
        self._active, self._problem = active, problem
        self._show()

    def _show(self):
        dl, state = vision_download.shared(), self._host.vision_state()
        text, download = "", False
        if not self._active:
            pass
        elif self._problem:
            text = self._problem
        elif dl.running():
            text = "Downloading…"
        elif state == vision.MISSING:
            text, download = f"What it needs isn't downloaded yet (about {dl.size_mb()} MB).", dl.available()
        elif state == vision.FAILED:
            text = "MediaPipe didn't load on this computer. Copy diagnostics has the details."
        elif state == vision.LOADING:
            text = "Getting ready…"
        self._label.setText(text)
        self.setVisible(bool(text))
        self._download_btn.setVisible(download)

    def _download(self):
        vision_download.shared().start()
        self._show()

    def _on_progress(self, done: int, size: int):
        if size and not self.isHidden() and not self._problem:
            self._label.setText(f"Downloading… {min(99, done * 100 // size)}%")
