import os
import sys
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import pytest


@pytest.fixture
def config_home(tmp_path, monkeypatch):
    """Point telescope.config at an isolated XDG_CONFIG_HOME/APPDATA for the
    duration of a test, and reload the module so its cached legacy-path
    constant is rebuilt against the fake HOME too."""
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
