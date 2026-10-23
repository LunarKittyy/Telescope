"""scripts/check_plugin.py: what a plugin author sees about their plugin, run as they'd run it."""

import os
import subprocess
import sys
from pathlib import Path

import pytest

DESKTOP = Path(__file__).resolve().parent.parent


def _check(folder) -> tuple:
    env = dict(os.environ, QT_QPA_PLATFORM="offscreen")
    out = subprocess.run([sys.executable, "scripts/check_plugin.py", str(folder)], env=env, cwd=DESKTOP,
                         capture_output=True, text=True, timeout=120)
    return out.returncode, out.stdout + out.stderr


def test_the_example_plugin_passes():
    code, out = _check(DESKTOP / "examples" / "plugins" / "black_and_white")
    assert code == 0, out
    assert "[FAIL]" not in out and "All good" in out


def test_a_broken_plugin_gets_each_problem_with_its_own_traceback(tmp_path):
    folder = tmp_path / "bad_one"
    folder.mkdir()
    (folder / "plugin.json").write_text('{"name": "bad_one", "api": 1, "class": "P"}')
    (folder / "__init__.py").write_text(
        "from telescope.plugin import TelescopePlugin\n\n"
        "class P(TelescopePlugin):\n"
        "    name = 'bad_one'\n\n"
        "    def process_frame(self, frame):\n        return frame[:, :, 0]\n\n"
        "    def get_config(self):\n        return {'when': object()}\n\n"
        "    def on_stream_start(self, url, ctrl):\n        raise KeyError('lens')\n")
    code, out = _check(folder)
    assert code == 1 and "3 problems to fix" in out
    assert "process_frame() has to return a BGR frame" in out and "JSON" in out
    assert f'{folder / "__init__.py"}", line 13, in on_stream_start' in out and "check_plugin.py" not in out


def test_a_plugin_that_wont_load_says_why(tmp_path):
    folder = tmp_path / "broken"
    folder.mkdir()
    (folder / "plugin.json").write_text('{"name": "broken", "api": 1, "class": "P"}')
    (folder / "__init__.py").write_text("import not_a_module_anywhere\n")
    code, out = _check(folder)
    assert code == 1 and "No module named 'not_a_module_anywhere'" in out, out


@pytest.mark.parametrize("interrupted", [False, True])
def test_checker_destroys_its_window_before_releasing_qt(qapp, monkeypatch, tmp_path, interrupted):
    from PyQt6 import sip
    from PyQt6.QtWidgets import QWidget
    from scripts import check_plugin
    from telescope import app

    shutdown = []

    class Window(QWidget):
        def _shut_down(self):
            shutdown.append(not sip.isdeleted(self))

    win = Window()
    child = QWidget(win)
    monkeypatch.setattr(app, "TelescopeWindow", lambda: win)

    def check_window(*_args):
        if interrupted:
            raise RuntimeError("setup failed")
        return  # a plugin that did not load returns early too

    monkeypatch.setattr(check_plugin, "_check_window", check_window)
    log = check_plugin._Logged("broken", tmp_path)
    if interrupted:
        with pytest.raises(RuntimeError, match="setup failed"):
            check_plugin.check_running(tmp_path, "broken", log)
    else:
        check_plugin.check_running(tmp_path, "broken", log)
    assert shutdown == [True]
    assert sip.isdeleted(win) and sip.isdeleted(child)
    assert not sip.isdeleted(qapp)
