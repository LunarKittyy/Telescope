"""Windows: asking to download adb from Google, or picking one the person already has, then doing it off the UI
thread (platform/adb_download.py)."""

import logging
import threading
from pathlib import Path

from PyQt6.QtCore import QObject, Qt, pyqtSignal
from PyQt6.QtWidgets import QFileDialog, QMessageBox

from telescope.platform import adb_download

logger = logging.getLogger(__name__)

GET_ADB_LINK = "get-adb"  # the href a label's "Get adb" link uses
PICK_ADB_LINK = "pick-adb"  # and its "pick one you already have" link


def confirm(parent) -> bool:
    box = QMessageBox(parent)
    box.setWindowTitle("Get adb")
    box.setIcon(QMessageBox.Icon.Question)
    box.setText("USB needs adb, Google's tool for talking to Android phones.")
    box.setInformativeText(
        f"Telescope can download it from Google (about {adb_download.APPROX_SIZE}) and keep it for your user. "
        "It's Google's software, so their Android SDK License applies to it: "
        f'<a href="{adb_download.LICENSE_URL}">read it here</a>.<br><br>'
        "Wi-Fi works without it.")
    box.setTextFormat(Qt.TextFormat.RichText)
    get = box.addButton("Agree and download", QMessageBox.ButtonRole.AcceptRole)
    box.addButton(QMessageBox.StandardButton.Cancel)
    box.exec()
    return box.clickedButton() is get


class AdbDownload(QObject):
    """One download at a time. progress and finished arrive on the UI thread."""

    progress = pyqtSignal(str)
    finished = pyqtSignal(bool, str)  # ok, adb's version or why it failed

    def __init__(self, parent=None, download=None):
        super().__init__(parent)
        self._download = download or adb_download.download_adb
        self.running = False
        self.finished.connect(self._done)

    def ask_and_start(self, window, starting=None) -> bool:
        """Ask first; False if it's already running or they said no. starting() runs just before the download."""
        if self.running or not confirm(window):
            return False
        if starting:
            starting()
        self.start()
        return True

    def pick_and_start(self, window, starting=None) -> bool:
        """Ask for an adb.exe or platform-tools zip and keep a copy; False if it's running or they cancelled."""
        if self.running:
            return False
        path, _ = QFileDialog.getOpenFileName(window, "Pick adb.exe or the platform-tools zip", "",
                                              "adb.exe or platform-tools zip (adb.exe *.zip)")
        if not path:
            return False
        if starting:
            starting()
        self.start(lambda progress: adb_download.use_own_adb(Path(path)))
        return True

    def start(self, job=None):
        self.running = True
        progress, finished, download = self.progress, self.finished, job or self._download

        def work():
            try:
                ok, detail = download(progress=progress.emit)
            except Exception:  # always report back, or the button stays off until a restart
                logger.exception("adb download failed")
                ok, detail = False, "Couldn't download adb. Try again."
            try:
                finished.emit(ok, detail)
            except RuntimeError:  # the dialog closed during the download
                pass

        threading.Thread(target=work, daemon=True).start()

    def _done(self, *_):
        self.running = False
