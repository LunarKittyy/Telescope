"""Windows: the Get adb download off the UI thread (widgets/adb.py)."""

import threading
import time

from telescope.widgets import adb
from telescope.widgets.adb import AdbDownload


def _wait_for(qapp, done, timeout=5.0):
    end = time.monotonic() + timeout
    while not done() and time.monotonic() < end:
        qapp.processEvents()
        time.sleep(0.01)
    assert done()


def test_asking_while_a_download_runs_doesn_t_ask_or_start_another(qapp, monkeypatch):
    asked, calls = [], []
    monkeypatch.setattr(adb, "confirm", lambda parent: asked.append(parent) or True)
    release = threading.Event()

    def download(progress):
        calls.append(1)
        release.wait(5)
        return True, "37.0.1"
    done = []
    widget = AdbDownload(download=download)
    widget.finished.connect(lambda ok, detail: done.append((ok, detail)))
    assert widget.ask_and_start(None)
    assert widget.running
    assert not widget.ask_and_start(None)
    assert len(asked) == 1
    release.set()
    _wait_for(qapp, lambda: done)
    assert calls == [1] and not widget.running


def test_a_download_that_raises_still_reports_back(qapp):
    def download(progress):
        raise RuntimeError("boom")
    done = []
    widget = AdbDownload(download=download)
    widget.finished.connect(lambda ok, detail: done.append((ok, detail)))
    widget.start()
    _wait_for(qapp, lambda: done)
    assert done == [(False, "Couldn't download adb. Try again.")]
    _wait_for(qapp, lambda: not widget.running)
