#!/usr/bin/env python3
"""Telescope Desktop — entry point."""

import argparse
import sys
import threading
from pathlib import Path

import update_guard

APP_DIR = Path(sys.executable if getattr(sys, "frozen", False) else __file__).resolve().parent
# Before anything else is imported: an update that was cut short may leave files that don't import together.
_right_version = update_guard.recover(APP_DIR, wait=15 if "--after-update" in sys.argv[1:] else 0)
if _right_version is not None:
    try:
        update_guard.launch(_right_version)
    except OSError:
        pass  # nothing more this copy can do; starting Telescope again runs whatever version is in place
    sys.exit(0)

# The new version has started fine this long after its window came up (or once it's closed normally).
_STARTED_FINE_MS = 3000

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
    # start.sh installs only when this stamp is out of date; dropping it makes the next launch install again
    try:
        Path(sys.prefix, ".telescope-requirements").unlink(missing_ok=True)
    except OSError:
        pass
    sys.exit(1)

from PyQt6.QtCore import QTimer
from PyQt6.QtWidgets import QApplication

from telescope import dev_profile, diagnostics
from telescope.app import (
    TelescopeWindow, acquire_single_instance, listen_for_raise,
)
from telescope.platform import IS_LINUX, autostart, virtual_mic
from telescope.plugins.browser_camera import BrowserCameraPlugin
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
    if getattr(sys, "frozen", False) and not IS_LINUX:
        import tempfile
        from telescope.platform import windows
        if windows.running_from_archive(APP_DIR, Path(tempfile.gettempdir())):
            windows.warn_running_from_archive()
            sys.exit(1)
    args, qt_argv = parse_args(sys.argv[1:])
    diagnostics.install(APP_DIR)
    app = QApplication([sys.argv[0]] + qt_argv)
    # Set at QApplication level so dialogs and window share icon.
    app.setWindowIcon(create_app_icon(64))
    if not dev_profile.active():  # a dev profile's window shouldn't group under the real app's dock icon
        app.setDesktopFileName(autostart.APP_ID)  # docks match the window to the menu entry and its icon
    if not autostart.is_dev_checkout() and not dev_profile.active():
        if IS_LINUX:
            autostart.update_menu_entry(lambda path: create_app_icon(256).pixmap(256, 256).save(str(path), "PNG"))
        autostart.refresh()

    srv = acquire_single_instance(wait=15 if args.after_update else 0)
    if srv is None:
        sys.exit(0)
    if IS_LINUX:
        # Only the copy holding the single-instance port gets here, so the source it finds is never a running one's
        virtual_mic.remove_stale_source(lambda cmd: virtual_mic._run(cmd, timeout=2))
    apply_theme(app)

    win = TelescopeWindow()
    win.register_plugin(SetupPlugin())
    win.register_plugin(ConnectionPlugin())
    win.register_plugin(CameraControlPlugin())
    win.register_plugin(BrowserCameraPlugin())  # after Camera: its card takes that place while it's picked
    win.register_plugin(StreamOutputPlugin())
    win.register_plugin(TransformsPlugin())
    win.register_plugin(MicrophonePlugin())
    win.register_plugin(PresetsPlugin())
    win.register_plugin(PreviewPlugin())
    win.register_plugin(OnboardingPlugin())  # after Preview: the stage listens for setup_needed
    win.register_plugin(MonitoringPlugin())
    win.register_plugin(UpdatesPlugin())
    win.register_plugin(WaitScreenPlugin())  # before Startup: dialogs head the settings menu, toggles follow
    win.register_plugin(StartupPlugin())
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

    def started_fine():
        if not started_fine.done:
            started_fine.done = True
            update_guard.confirm(APP_DIR)
            # Every start, not just after an update: an old version's files can still be locked the first time.
            clean_up_after_update()
    started_fine.done = False
    QTimer.singleShot(_STARTED_FINE_MS, started_fine)
    app.aboutToQuit.connect(started_fine)

    ret = app.exec()
    srv.close()
    sys.exit(ret)


if __name__ == "__main__":
    main()
