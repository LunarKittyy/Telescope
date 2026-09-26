import atexit
import os
import shutil
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
    yield app
