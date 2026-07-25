#!/usr/bin/env python3
"""Telescope Desktop — entry point."""

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

if _missing:
    print(f"Missing: pip install {' '.join(_missing)}", file=sys.stderr)
    sys.exit(1)

from PyQt6.QtWidgets import QApplication

from telescope.app import (
    TelescopeWindow, acquire_single_instance, listen_for_raise,
)
from telescope.plugins.camera_control import CameraControlPlugin
from telescope.plugins.connection import ConnectionPlugin
from telescope.plugins.monitoring import MonitoringPlugin
from telescope.plugins.preview import PreviewPlugin
from telescope.plugins.setup import SetupPlugin
from telescope.plugins.stream_output import StreamOutputPlugin
from telescope.plugins.transforms import TransformsPlugin
from telescope.theme import apply_theme


def main():
    app = QApplication(sys.argv)

    srv = acquire_single_instance()
    if srv is None:
        sys.exit(0)

    apply_theme(app)

    win = TelescopeWindow()
    win.register_plugin(SetupPlugin())
    win.register_plugin(ConnectionPlugin())
    win.register_plugin(CameraControlPlugin())
    win.register_plugin(StreamOutputPlugin())
    win.register_plugin(TransformsPlugin())
    win.register_plugin(PreviewPlugin())
    win.register_plugin(MonitoringPlugin())
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
