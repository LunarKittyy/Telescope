"""Put on PYTHONPATH with TELESCOPE_T0 set (a CLOCK_MONOTONIC time in ns) to have the Python app print when its first
widget (the splash) and its main window first drew, the same way telescope-rs does. Changes nothing else."""

import os
import time

_T0 = os.environ.get("TELESCOPE_T0")

if _T0:
    from PyQt6.QtCore import QTimer
    from PyQt6.QtWidgets import QMainWindow, QWidget

    def _report(label):
        print(f"{label} {(time.monotonic_ns() - int(_T0)) / 1e6:.1f}", flush=True)

    def _wrap(cls, label):
        original = cls.show
        done = []

        def show(self, *args):
            original(self, *args)
            if not done and (cls is not QWidget or self.isWindow()):
                done.append(True)
                QTimer.singleShot(0, lambda: _report(label))  # after the event loop has painted it
        cls.show = show

    _wrap(QWidget, "first-widget-ms")
    _wrap(QMainWindow, "first-paint-ms")
