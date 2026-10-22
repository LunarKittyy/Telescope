"""The vision pack's download, one for the whole app: the Plugins dialog and the cards that need it share it."""
import threading
from typing import Optional

from PyQt6.QtCore import QObject, QCoreApplication, pyqtSignal

from telescope import plugin_loader, vision, vision_pack


class VisionDownload(QObject):
    progress = pyqtSignal(int, int)  # bytes so far, bytes in all (0: not known)
    finished = pyqtSignal(str)  # "" when it's ready, otherwise what went wrong

    def __init__(self, parent=None):
        super().__init__(parent)
        self._thread: Optional[threading.Thread] = None
        self._cancel = threading.Event()
        self.waiting = ""  # the optional plugin that gets switched on once it's done
        self.error = ""  # what went wrong last time, for the plugin that was waiting
        self.failed_for = ""
        self.finished.connect(self._done)  # connected first, so the plugin is on before anyone else hears

    def ready(self) -> bool:
        return vision_pack.installed(vision.vision_dir())

    def available(self) -> bool:
        """Whether there's a pack for this computer at all."""
        return vision_pack.asset_name() is not None

    def running(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    def size_mb(self) -> int:
        return max(1, round(vision_pack.download_size() / 1e6))

    def start(self, switch_on: str = ""):
        """Download in the background, then switch on the optional plugin switch_on; finished says how it went.
        Asking while one runs only changes which plugin waits for it."""
        self.waiting = switch_on or self.waiting
        self.error = self.failed_for = ""
        if self.running():
            return
        self._cancel.clear()
        self._thread = threading.Thread(target=self._run, daemon=True, name="vision-download")
        self._thread.start()

    def cancel(self):
        self._cancel.set()

    def _run(self):
        try:
            vision_pack.download(vision.vision_dir(), progress=self.progress.emit, cancelled=self._cancel.is_set)
        except vision_pack.PackError as exc:
            self.finished.emit(str(exc))
            return
        self.finished.emit("")

    def _done(self, error: str):
        name, self.waiting = self.waiting, ""
        if error:
            self.error, self.failed_for = error, name
        elif name:
            try:
                plugin_loader.set_optional_on(name, True)
            except OSError as exc:
                self.error, self.failed_for = f"The download worked but the switch couldn't be saved: {exc}", name

    def remove(self):
        vision_pack.remove(vision.vision_dir())


_shared: Optional[VisionDownload] = None


def shared() -> VisionDownload:
    """The app's one VisionDownload (it lives as long as the app does)."""
    global _shared
    if _shared is None:
        _shared = VisionDownload(QCoreApplication.instance())
    return _shared
