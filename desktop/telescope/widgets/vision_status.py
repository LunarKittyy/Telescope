"""The status row on cards that use vision: why nothing's happening yet, with a Download button when that's why."""
from PyQt6.QtWidgets import QHBoxLayout, QPushButton, QWidget

from telescope import vision
from telescope.widgets import vision_download
from telescope.widgets.common import wrapped_note
from telescope.i18n import tr


class VisionStatus(QWidget):
    """Hidden while there's nothing to say; refresh() after anything on the card that changes what it would say."""

    def __init__(self, host, bus, parent=None):
        super().__init__(parent)
        self._host = host
        self._active, self._problem, self._optional = False, "", False
        self._label = wrapped_note("", "status_warn")
        self._download_btn = QPushButton(tr("Download"))
        self._download_btn.clicked.connect(self._download)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        lay.addWidget(self._label, 1)
        lay.addWidget(self._download_btn)
        # Methods, not lambdas: the app's bus and download outlive the card, and a method lets go when it goes
        bus.vision_state_changed.connect(self._on_changed)
        dl = vision_download.shared()
        dl.progress.connect(self._on_progress)
        dl.finished.connect(self._on_changed)
        self._show()

    def text(self) -> str:
        return self._label.text()

    def refresh(self, active: bool, problem: str = "", optional: bool = False):
        """active: the card is switched to something that needs vision; problem: the card's own reason, shown first;
        optional: it works without vision too, only better with it."""
        self._active, self._problem, self._optional = active, problem, optional
        self._show()

    def _show(self):
        dl, state = vision_download.shared(), self._host.vision_state()
        text, download = "", False
        if not self._active:
            pass
        elif self._problem:
            text = self._problem
        elif dl.running():
            text = tr("Downloading…")
        elif state == vision.MISSING and self._optional:
            text, download = tr("Cleaner edges with a one-time download (about {mb} MB).", mb=dl.size_mb()), dl.available()
        elif state == vision.MISSING:
            text, download = tr("Needs a one-time download (about {mb} MB).", mb=dl.size_mb()), dl.available()
        elif self._optional:
            pass
        elif state == vision.FAILED:
            text = tr("MediaPipe didn't load on this computer. Copy diagnostics has the details.")
        elif state == vision.LOADING:
            text = tr("Getting ready…")
        self._label.setText(text)
        self.setVisible(bool(text))
        self._download_btn.setVisible(download)

    def _on_changed(self, *_):
        self._show()

    def _download(self):
        vision_download.shared().start()
        self._show()

    def _on_progress(self, done: int, size: int):
        if size and not self.isHidden() and not self._problem:
            self._label.setText(tr("Downloading… {percent}%", percent=min(99, done * 100 // size)))
