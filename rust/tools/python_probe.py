#!/usr/bin/env python3
"""Runs the Python app's desktop/main.py and prints "first-paint-ms N" (against TELESCOPE_T0, a CLOCK_MONOTONIC reading in
nanoseconds, as the Rust app does) when the main window gets its first paint event. For measure.py startup.

  python_probe.py [main.py options]      run from desktop/ with that checkout's venv python"""

import os
import runpy
import sys
import time
from pathlib import Path

DESKTOP = Path.cwd()
sys.path.insert(0, str(DESKTOP))
sys.argv = [str(DESKTOP / "main.py")] + sys.argv[1:]
T0 = int(os.environ.get("TELESCOPE_T0", "0"))

from PyQt6.QtCore import QEvent, QObject  # noqa: E402
from PyQt6.QtWidgets import QApplication, QWidget  # noqa: E402


class FirstPaint(QObject):
    done = False

    def eventFilter(self, obj, event):
        if not self.done and event.type() == QEvent.Type.Paint and isinstance(obj, QWidget) \
                and type(obj.window()).__name__ == "TelescopeWindow":
            self.done = True
            print(f"first-paint-ms {(time.monotonic_ns() - T0) / 1e6:.1f}", flush=True)
        return False


_init = QApplication.__init__
_keep = []


def init(self, *args, **kwargs):
    _init(self, *args, **kwargs)
    watcher = FirstPaint()
    _keep.append(watcher)
    self.installEventFilter(watcher)


QApplication.__init__ = init
runpy.run_path(str(DESKTOP / "main.py"), run_name="__main__")
