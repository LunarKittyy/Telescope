"""Startup must own the app's lock before changing or importing the selected PyAV."""

import json
import os
from pathlib import Path
import subprocess
import sys

import pytest


DESKTOP = Path(__file__).resolve().parents[1]


def _run(tmp_path, code, exit_code=True):
    env = {**os.environ, "XDG_DATA_HOME": str(tmp_path / "data"), "XDG_CONFIG_HOME": str(tmp_path / "config"),
           "TELESCOPE_DEV_PROFILE": str(tmp_path / "profile"), "QT_QPA_PLATFORM": "offscreen"}
    result = subprocess.run([sys.executable, "-c", code], cwd=DESKTOP, env=env, capture_output=True, text=True, timeout=15)
    if exit_code:
        assert result.returncode == 0, result.stderr
    return result


@pytest.mark.parametrize("args", [["--list-actions"], ["--action", "microphone.mute", "mode=toggle"], []])
def test_commands_and_duplicate_launches_leave_a_staged_build_alone(tmp_path, args):
    root = tmp_path / "data" / "Telescope" / "pyav-vaapi"
    final, ready = root / "18.1.0", root / ".ready-18.1.0"
    for folder, marker in ((final, "old"), (ready, "new")):
        (folder / "av").mkdir(parents=True)
        (folder / "av" / marker).touch()
    (root / "choice.json").write_text(json.dumps({"kind": "build", "path": str(final)}))
    _run(tmp_path, f"""
import sys
import main
main.send_action = lambda request: (True, 'done')
main.acquire_single_instance = lambda **kwargs: None
sys.argv = ['main.py'] + {args!r}
main.main()
""")
    assert (final / "av" / "old").exists() and (ready / "av" / "new").exists()


def test_new_instance_activates_pyav_after_getting_the_lock_and_before_importing_it(tmp_path):
    _run(tmp_path, """
import sys
import main
from telescope import pyav_vaapi
owned = False
def acquire(**kwargs):
    global owned
    owned = True
    return object()
class Activated(Exception):
    pass
def activate():
    assert owned
    assert 'av' not in sys.modules
    raise Activated()
main.acquire_single_instance = acquire
main.write_control_key = lambda: 'key'
pyav_vaapi.activate = activate
sys.argv = ['main.py']
try:
    main.main()
except Activated:
    pass
else:
    raise AssertionError('startup did not activate PyAV')
""")


def test_the_window_comes_up_without_loading_what_only_a_stream_needs(tmp_path):
    # Each file loaded is seconds of Windows checking it on a first start, so these wait until the window is up
    result = _run(tmp_path, """
import os
import socket
import sys
from PyQt6.QtWidgets import QApplication
import main
# Checked where the app would start its loop: letting main() return drops its window under live threads (a crash on Windows)
def window_up(self):
    print('loaded', [name for name in ('av', 'cryptography', 'sounddevice', 'aiortc') if name in sys.modules], flush=True)
    os._exit(0)
QApplication.exec = window_up
main.acquire_single_instance = lambda **kwargs: socket.socket()
main.listen_for_raise = lambda *args, **kwargs: None
sys.argv = ['main.py']
main.main()
""")
    assert "loaded []" in result.stdout, result.stdout + result.stderr


def test_the_warm_up_helper_loads_pyav_and_quits_without_starting_the_app(tmp_path):
    env = {**os.environ, "XDG_DATA_HOME": str(tmp_path / "data"), "XDG_CONFIG_HOME": str(tmp_path / "config"),
           "TELESCOPE_DEV_PROFILE": str(tmp_path / "profile"), "QT_QPA_PLATFORM": "offscreen"}
    code = "import sys, runpy; sys.argv = ['main.py', '--warm-up']\n" \
           "try:\n    runpy.run_path('main.py', run_name='__main__')\nexcept SystemExit as e:\n    assert e.code == 0\n" \
           "assert 'av' in sys.modules and 'PyQt6.QtWidgets' not in sys.modules"
    result = subprocess.run([sys.executable, "-c", code], cwd=DESKTOP, env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stderr
