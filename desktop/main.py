#!/usr/bin/env python3
"""PhoneCam Desktop — entry point."""

import sys
import threading

_missing = []
try:    from PyQt6.QtCore import Qt
except ImportError: _missing.append("PyQt6")
try:    import cv2
except ImportError: _missing.append("opencv-python")
try:    import numpy as np
except ImportError: _missing.append("numpy")
try:    import pyvirtualcam
except ImportError: _missing.append("pyvirtualcam")
try:    import qt_material
except ImportError: _missing.append("qt-material")

if _missing:
    print(f"Missing: pip install {' '.join(_missing)}", file=sys.stderr)
    sys.exit(1)

from PyQt6.QtWidgets import QApplication
from qt_material import apply_stylesheet

from phonecam.app import (
    EXTRA_QSS, PhoneCamWindow, acquire_single_instance, listen_for_raise,
)
from phonecam.plugins.camera_control import CameraControlPlugin
from phonecam.plugins.connection import ConnectionPlugin
from phonecam.plugins.monitoring import MonitoringPlugin
from phonecam.plugins.setup import SetupPlugin
from phonecam.plugins.stream_output import StreamOutputPlugin
from phonecam.plugins.transforms import TransformsPlugin


def main():
    app = QApplication(sys.argv)

    srv = acquire_single_instance()
    if srv is None:
        sys.exit(0)

    apply_stylesheet(app, theme='dark_blue.xml')
    app.setStyleSheet(app.styleSheet() + EXTRA_QSS)

    win = PhoneCamWindow()
    win.register_plugin(ConnectionPlugin())
    win.register_plugin(CameraControlPlugin())
    win.register_plugin(StreamOutputPlugin())
    win.register_plugin(TransformsPlugin())
    win.register_plugin(MonitoringPlugin())
    win.register_plugin(SetupPlugin())
    win.apply_saved_config()
    win.show()

    threading.Thread(
        target=listen_for_raise,
        args=(srv, win._sig_raise.emit),
        daemon=True,
    ).start()

    ret = app.exec()
    srv.close()
    sys.exit(ret)


if __name__ == "__main__":
    main()
