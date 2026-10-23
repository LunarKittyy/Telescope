from types import SimpleNamespace

import pytest
from PyQt6 import sip
from PyQt6.QtWidgets import QWidget

from scripts import smoke_check
from telescope import app


@pytest.mark.parametrize("failure", [None, "register", "config", "shutdown"])
def test_construction_check_shuts_down_and_destroys_its_window(qapp, monkeypatch, failure):
    shutdown = []

    class Window(QWidget):
        def __init__(self):
            super().__init__()
            self._plugins = []

        def register_plugin(self, plugin):
            self._plugins.append(plugin)
            if failure == "register":
                raise RuntimeError("register failed")

        def apply_saved_config(self):
            if failure == "config":
                raise RuntimeError("config failed")

        def _plugin(self, _name):
            return SimpleNamespace(actions=lambda: [], backend=None, shutdown=lambda: None)

        def _shut_down(self):
            shutdown.append(not sip.isdeleted(self))
            if failure == "shutdown":
                raise RuntimeError("shutdown failed")

    win = Window()
    child = QWidget(win)
    monkeypatch.setattr(app, "TelescopeWindow", lambda: win)
    if failure:
        with pytest.raises(RuntimeError, match=f"{failure} failed"):
            smoke_check.check_app_construction()
    else:
        assert "15 plugins registered" in smoke_check.check_app_construction()
    assert shutdown == [True]
    assert sip.isdeleted(win) and sip.isdeleted(child)
    assert not sip.isdeleted(qapp)
