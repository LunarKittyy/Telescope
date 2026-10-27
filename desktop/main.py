#!/usr/bin/env python3
"""Telescope Desktop — entry point."""

import argparse
import importlib.util
import os
import sys
import threading
from pathlib import Path
from typing import Optional

import update_guard

WARM_UP = "--warm-up"
if sys.argv[1:] == [WARM_UP]:
    # _preload's helper process: Windows scans these files here, holding this process's loader lock instead of the app's
    try:
        import av  # noqa: F401
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM  # noqa: F401
    except ImportError:
        pass
    sys.exit(0)

# Read by OpenCV as it loads, for webcams: one that stops sending drops after 3 s rather than 10
os.environ.setdefault("OPENCV_VIDEOIO_V4L_SELECT_TIMEOUT", "3")
# Media Foundation's hardware transforms can make opening some webcams take many seconds
os.environ.setdefault("OPENCV_VIDEOIO_MSMF_ENABLE_HW_TRANSFORMS", "0")
if sys.platform == "win32":
    os.environ.setdefault("QT_MEDIA_BACKEND", "windows")  # the webcam list needs no FFmpeg, which the bundle leaves out

APP_DIR = Path(sys.executable if getattr(sys, "frozen", False) else __file__).resolve().parent
# Before anything else is imported: an update that was cut short may leave files that don't import together.
_after_update = "--after-update" in sys.argv[1:]
# legacy: the copy that installed this update may be one from before the per-user lock
_right_version = update_guard.recover(APP_DIR, wait=15 if _after_update else 0,
                                      running_elsewhere=lambda: update_guard.another_copy_running(legacy=_after_update))
if _right_version is not None:
    try:
        update_guard.launch(_right_version)
    except OSError:
        pass  # nothing more this copy can do; starting Telescope again runs whatever version is in place
    sys.exit(0)
update_guard.confirm_on_terminate(APP_DIR)


def _chosen_language() -> str:
    """The language picked in Settings > General, "" for the system's."""
    from telescope.config import load_config
    setup = load_config().get("plugin_configs", {}).get("setup")
    lang = setup.get("language") if isinstance(setup, dict) else ""
    return lang if isinstance(lang, str) else ""


from telescope import splash  # noqa: E402
splash.show_text(_chosen_language)  # before Qt, which most of a first start's wait is spent loading

# The new version has started fine this long after its window came up (or once it's closed normally).
_STARTED_FINE_MS = 3000

_missing = []
try:    from PyQt6.QtCore import Qt
except ImportError: _missing.append("PyQt6")
# Only looked for here, so the log's START comes sooner: a first start on Windows spends seconds on each file it loads
for _module, _package in (("cv2", "opencv-python-headless"), ("numpy", "numpy"), ("pyvirtualcam", "pyvirtualcam"),
                          ("ifaddr", "ifaddr"), ("zeroconf", "zeroconf")):
    if importlib.util.find_spec(_module) is None:
        _missing.append(_package)

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

from telescope import dev_profile, diagnostics, i18n
from telescope.single_instance import (
    RESTARTED, acquire_single_instance, listen_for_raise, send_action, write_control_key,
)
from telescope.platform import IS_LINUX, autostart, virtual_mic
from telescope.theme import apply_theme
from telescope.updates import MOVED_FROM, clean_up_after_update, finish_move, note_installed_version, pending_move
from telescope.widgets.common import create_app_icon


def parse_args(argv):
    """Telescope's own options; everything else (Qt's -style, -platform, ...) goes to Qt."""
    parser = argparse.ArgumentParser(prog="telescope", add_help=False)
    parser.add_argument("--after-update", action="store_true",
                        help="started by the updater: wait for the old copy to exit, then tidy up")
    parser.add_argument(RESTARTED, action="store_true",
                        help="started by Restart now (Plugins): wait for the old copy to exit")
    parser.add_argument("--minimized", action="store_true",
                        help="start in the tray (used when opening at sign-in)")
    parser.add_argument(MOVED_FROM, dest="moved_from", metavar="FOLDER",
                        help="started by an unzipped copy that installed this one: delete that copy once started")
    parser.add_argument("--action", nargs="+", metavar=("ID", "NAME=VALUE"),
                        help="run a shortcut action in the running Telescope, e.g. --action microphone.mute "
                             "mode=toggle (for desktops without global shortcuts)")
    parser.add_argument("--list-actions", action="store_true",
                        help="list the actions --action takes, with their settings")
    return parser.parse_known_args(argv)


def action_request(words: list) -> dict:
    """--action's words as a request: the id, then name=value settings (the running copy reads the numbers)."""
    params = {}
    for word in words[1:]:
        name, sep, value = word.partition("=")
        if not sep or not name:
            raise ValueError(f"{word!r} isn't NAME=VALUE")
        params[name] = value
    return {"action": words[0], "params": params}


def _attach_console():
    """The packaged Windows app has no console, so print() goes nowhere: borrow the one it was started from."""
    if sys.platform != "win32" or not getattr(sys, "frozen", False):
        return
    try:
        import ctypes
        if ctypes.windll.kernel32.AttachConsole(-1):  # ATTACH_PARENT_PROCESS
            sys.stdout = open("CONOUT$", "w", encoding="utf-8")
            sys.stderr = open("CONOUT$", "w", encoding="utf-8")
    except (OSError, AttributeError):
        pass


def run_action(args) -> int:
    """--action / --list-actions: talk to the running copy, print what it says, and exit."""
    _attach_console()
    i18n.load(i18n.resolve(_chosen_language()))
    try:
        request = {"list": True} if args.list_actions else action_request(args.action)
    except ValueError as exc:
        print(f"telescope: {exc}", file=sys.stderr)
        return 2
    ok, text = send_action(request)
    print(text, file=sys.stdout if ok else sys.stderr)
    return 0 if ok else 1


def _watch_dev_plugins(win) -> Optional[QTimer]:
    """dev_desktop.py --plugin: restart when one of the plugin's files is saved, so the change shows right away."""
    folders = dev_profile.plugin_folders()
    if not folders:
        return None
    last = dev_profile.files_stamp(folders)
    timer = QTimer()

    def look():
        nonlocal last
        now = dev_profile.files_stamp(folders)
        if now != last:
            print(f"Saw a change in {', '.join(f.name for f in folders)}", file=sys.stderr, flush=True)
            timer.stop()
            win.restart_app()

    timer.timeout.connect(look)
    timer.start(700)
    return timer


def _warm_up():
    """Runs the app once more with --warm-up and waits for it. Windows scans each new file while it loads and holds the
    process's loader lock meanwhile, which every new thread needs, so loading PyAV here first would freeze the window
    whenever it starts one. Once that helper has had them scanned, loading them here is quick."""
    import subprocess
    env = dict(os.environ, PYINSTALLER_SUPPRESS_SPLASH_SCREEN="1")
    try:
        subprocess.run([sys.executable, WARM_UP], env=env, timeout=600, creationflags=subprocess.CREATE_NO_WINDOW)
    except (OSError, subprocess.SubprocessError):
        pass


def _preload():
    """What the first stream needs and a Windows first start loads slowly (PyAV, its graphics card, cryptography), once the window is up."""
    from telescope import h264_reader, udp_reader
    if getattr(sys, "frozen", False) and sys.platform == "win32":
        h264_reader.load_av(warm=_warm_up)
    h264_reader.hardware_device()
    udp_reader.preload()


def main():
    if getattr(sys, "frozen", False) and not IS_LINUX:
        import tempfile
        from telescope.platform import windows
        if windows.running_from_archive(APP_DIR, Path(tempfile.gettempdir())):
            splash.close()
            windows.warn_running_from_archive()
            sys.exit(1)
        windows.use_installed_app_id(APP_DIR)
    args, qt_argv = parse_args(sys.argv[1:])
    if args.action or args.list_actions:
        splash.close()
        sys.exit(run_action(args))
    diagnostics.install(APP_DIR)
    import cv2
    cv2.setNumThreads(2)  # 16 threads spin more than they work on per-frame jobs this small
    if IS_LINUX:
        from telescope.platform.linux import limit_malloc_arenas
        limit_malloc_arenas()  # before Qt and the stream start their threads
        # pip's Qt has no KDE or GNOME integration, so file pickers would be Qt's own (no previews) without the portal
        if os.environ.get("QT_QPA_PLATFORMTHEME") not in ("gtk3", "xdgdesktopportal"):  # e.g. "kde", which it lacks
            os.environ["QT_QPA_PLATFORMTHEME"] = "xdgdesktopportal"
    app = QApplication([sys.argv[0]] + qt_argv)
    i18n.load(i18n.resolve(_chosen_language()))  # before any window or dialog is built
    # Set at QApplication level so dialogs and window share icon.
    app.setWindowIcon(create_app_icon(64))
    if not dev_profile.active():  # a dev profile's window shouldn't group under the real app's dock icon
        app.setDesktopFileName(autostart.APP_ID)  # docks match the window to the menu entry and its icon
    if not autostart.is_dev_checkout() and not dev_profile.active():
        if IS_LINUX:
            autostart.update_menu_entry(lambda path: create_app_icon(256).pixmap(256, 256).save(str(path), "PNG"))
        autostart.refresh()

    srv = acquire_single_instance(wait=15 if args.after_update or args.restarted else 0)
    if srv is None:
        sys.exit(0)
    control_key = write_control_key()  # straight away: an --action from a sign-in script mustn't find the last run's

    # Only the new instance may replace a staged build. Activate it before the window or plugins import av.
    from telescope import pyav_vaapi
    pyav_vaapi.activate()
    from telescope.app import TelescopeWindow
    from telescope.plugins import builtin_plugins

    if IS_LINUX:
        # Only the copy holding the single-instance port gets here, so the source it finds is never a running one's
        virtual_mic.remove_stale_source(lambda cmd: virtual_mic._run(cmd, timeout=2))
    apply_theme(app)

    win = TelescopeWindow()
    win.load_plugins(builtin_plugins())
    win.apply_saved_config()
    if args.minimized:
        win.start_hidden()
    else:
        win.show()
    splash.close()
    QTimer.singleShot(1500, lambda: threading.Thread(target=_preload, daemon=True).start())
    watch = _watch_dev_plugins(win)  # noqa: F841 (kept alive until quitting)
    if dev_profile.resume_stream():
        QTimer.singleShot(0, lambda: win.start_stream(interactive=False))

    threading.Thread(
        target=listen_for_raise,
        args=(srv, win._sig_raise.emit, win.run_remote_action, control_key),
        daemon=True,
    ).start()

    def started_fine():
        if not started_fine.done:
            started_fine.done = True
            update_guard.confirm(APP_DIR)
            # Every start, not just after an update: an old version's files can still be locked the first time.
            clean_up_after_update()
            if not IS_LINUX and getattr(sys, "frozen", False):
                note_installed_version()
            # Only now: had this copy failed to start, the old one would still be there to use
            moved_from = Path(args.moved_from) if args.moved_from else pending_move(APP_DIR)
            if moved_from is not None:
                # Not a daemon, so quitting straight away still lets it finish (a few seconds at most)
                threading.Thread(target=finish_move, args=(moved_from, APP_DIR)).start()
    started_fine.done = False
    # Python only runs a signal handler when the interpreter gets control, which Qt's loop doesn't give it by itself
    wake = QTimer()
    wake.timeout.connect(lambda: None)
    wake.start(250)
    QTimer.singleShot(_STARTED_FINE_MS, started_fine)
    app.aboutToQuit.connect(started_fine)

    ret = app.exec()
    srv.close()
    sys.exit(ret)


if __name__ == "__main__":
    main()
