#!/usr/bin/env python3
"""Telescope Desktop — entry point."""

import argparse
import sys
import threading
from pathlib import Path

_missing = []
try:    from PyQt6.QtCore import Qt
except ImportError: _missing.append("PyQt6")
try:    import cv2
except ImportError: _missing.append("opencv-python-headless")
try:    import numpy as np
except ImportError: _missing.append("numpy")
try:    import pyvirtualcam
except ImportError: _missing.append("pyvirtualcam")
try:    import ifaddr
except ImportError: _missing.append("ifaddr")
try:    import zeroconf
except ImportError: _missing.append("zeroconf")

if _missing:
    print(f"Missing: pip install {' '.join(_missing)}", file=sys.stderr)
    sys.exit(1)

from PyQt6.QtWidgets import QApplication

from telescope import diagnostics
from telescope.app import (
    TelescopeWindow, acquire_single_instance, listen_for_raise,
)
from telescope.platform import IS_LINUX, autostart
from telescope.plugins.camera_control import CameraControlPlugin
from telescope.plugins.connection import ConnectionPlugin
from telescope.plugins.microphone import MicrophonePlugin
from telescope.plugins.monitoring import MonitoringPlugin
from telescope.plugins.onboarding import OnboardingPlugin
from telescope.plugins.presets import PresetsPlugin
from telescope.plugins.preview import PreviewPlugin
from telescope.plugins.setup import SetupPlugin
from telescope.plugins.startup import StartupPlugin
from telescope.plugins.wait_screen import WaitScreenPlugin
from telescope.plugins.stream_output import StreamOutputPlugin
from telescope.plugins.transforms import TransformsPlugin
from telescope.plugins.updates import UpdatesPlugin
from telescope.theme import apply_theme
from telescope.updates import clean_up_after_update
from telescope.widgets.common import create_app_icon


def parse_args(argv):
    """Telescope's own options; everything else (Qt's -style, -platform, ...) goes to Qt."""
    parser = argparse.ArgumentParser(prog="telescope", add_help=False)
    parser.add_argument("--after-update", action="store_true",
                        help="started by the updater: wait for the old copy to exit, then tidy up")
    parser.add_argument("--minimized", action="store_true",
                        help="start in the tray (used when opening at sign-in)")
    return parser.parse_known_args(argv)


def main():
    args, qt_argv = parse_args(sys.argv[1:])
    app_dir = Path(sys.executable if getattr(sys, "frozen", False) else __file__).resolve().parent
    diagnostics.install(app_dir)
    app = QApplication([sys.argv[0]] + qt_argv)
    # Set at QApplication level so dialogs and window share icon.
    app.setWindowIcon(create_app_icon(64))
    app.setDesktopFileName(autostart.APP_ID)  # docks match the window to the menu entry and its icon
    if IS_LINUX and not autostart.is_dev_checkout():
        autostart.update_menu_entry(lambda path: create_app_icon(256).pixmap(256, 256).save(str(path), "PNG"))

    srv = acquire_single_instance(wait=15 if args.after_update else 0)
    if srv is None:
        sys.exit(0)
    if args.after_update:
        clean_up_after_update()

    apply_theme(app)

    win = TelescopeWindow()
    win.register_plugin(SetupPlugin())
    win.register_plugin(ConnectionPlugin())
    win.register_plugin(CameraControlPlugin())
    win.register_plugin(StreamOutputPlugin())
    win.register_plugin(TransformsPlugin())
    win.register_plugin(MicrophonePlugin())
    win.register_plugin(PresetsPlugin())
    win.register_plugin(PreviewPlugin())
    win.register_plugin(OnboardingPlugin())  # after Preview: the stage listens for setup_needed
    win.register_plugin(MonitoringPlugin())
    win.register_plugin(UpdatesPlugin())
    win.register_plugin(StartupPlugin())
    win.register_plugin(WaitScreenPlugin())  # after Startup: its menu entry goes under Startup's
    win.apply_saved_config()
    if args.minimized:
        win.start_hidden()
    else:
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
