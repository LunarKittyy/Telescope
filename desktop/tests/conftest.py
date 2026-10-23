import atexit
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

# A save that fires after its test (a window's debounced save timer) must never reach the real config.
_CONFIG_HOME = tempfile.mkdtemp(prefix="telescope-tests-")
atexit.register(shutil.rmtree, _CONFIG_HOME, ignore_errors=True)
os.environ["XDG_CONFIG_HOME"] = os.path.join(_CONFIG_HOME, "xdg")
os.environ["APPDATA"] = os.path.join(_CONFIG_HOME, "appdata")

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import pytest


@pytest.fixture(autouse=True)
def _no_desktop_notifications(monkeypatch):
    """A test never pops a real notification up on the desktop it runs on (notify-send is a no-op here)."""
    class Popen(subprocess.Popen):  # a class, since modules imported during a test subclass it (asyncio on Windows)
        def __init__(self, args, *a, **kw):
            if isinstance(args, (list, tuple)) and args and Path(str(args[0])).name == "notify-send":
                args, a, kw = [sys.executable, "-c", ""], (), {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}
            super().__init__(args, *a, **kw)
    monkeypatch.setattr(subprocess, "Popen", Popen)


@pytest.fixture
def config_home(tmp_path, monkeypatch):
    """Reload module to pick up monkeypatched env vars instead of using real home."""
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg"))
    monkeypatch.setenv("APPDATA", str(tmp_path / "appdata"))
    monkeypatch.setattr(Path, "home", lambda: tmp_path / "home")
    import importlib
    import telescope.config as config
    importlib.reload(config)
    yield config
    importlib.reload(config)


@pytest.fixture(scope="session")
def qapp():
    from PyQt6.QtWidgets import QApplication
    app = QApplication.instance() or QApplication([])
    # Applied before any test makes a widget: doing it later restyles the ones still alive, which crashes Qt, and
    # whether that happens would depend on which tests share a process (pytest -n) instead of being the same every run
    from telescope.theme import apply_theme
    apply_theme(app)
    yield app


@pytest.fixture
def qt_widgets(qapp):
    """Own registered widget roots until their deferred deletion finishes."""
    from PyQt6 import sip
    from PyQt6.QtCore import QEvent
    widgets = []

    def keep(widget):
        widgets.append(widget)
        return widget

    yield keep
    for widget in reversed(widgets):
        if not sip.isdeleted(widget):
            widget.close()
            widget.deleteLater()
    qapp.sendPostedEvents(None, QEvent.Type.DeferredDelete)
    qapp.processEvents()


@pytest.fixture(autouse=True)
def _no_locked_camera(monkeypatch):
    """Whatever the real virtual camera is doing on this machine stays out of the tests."""
    import telescope.vcam as vcam
    monkeypatch.setattr(vcam, "locked_size", lambda *_: None)
