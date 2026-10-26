"""Scenes for setup, onboarding, platform helpers, VA-API and adb: see translation_scenes.py for how scenes are registered.

Every scene runs the real code (the Setup plugin's panel, the onboarding steps, the platform functions) against a
pretend system: what would reach the network, the registry or the package manager is swapped for a fake that answers
the way a particular machine would, so the text on screen is the one the app would really say there.
"""

import contextlib
import hashlib
import io
import os
import shutil
import sys
import threading
import urllib.error
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from scripts import translation_scenes as _scenes
from scripts.translation_scenes import (Joined, Tagged, bubble, capture_notifications, grow, hold_dialogs, main_window, remember,  # noqa: F401
                                        scene, shown, use_scratch_profile)


# ── Helpers ──────────────────────────────────────────────────────────────────

def nested_texts(stack):
    """A text put into another one with tr("...{name}", name=other) keeps its own place on screen as well, and the
    modules that were imported before the worker tagged texts (the platform ones) tag theirs too."""
    from telescope import i18n
    tagging = i18n.tr
    for name, module in list(sys.modules.items()):
        plain = getattr(module, "tr", None)
        if name.startswith("telescope") and getattr(plain, "__module__", "") == "telescope.i18n" and plain is not tagging:
            stack.enter_context(mock.patch.object(module, "tr", tagging))
    original_new, original_parts, original_list = Tagged.__new__, _scenes.parts_of, i18n.and_list

    def new(cls, text, source, ctx, values):
        made = original_new(cls, text, source, ctx, values)
        made.inside = [part for value in values.values() for part in _scenes.parts_of(value)]
        return made

    def parts_of(text):
        found = original_parts(text)
        return found + [part for each in found for part in getattr(each, "inside", [])]

    def and_list(words):
        words = list(words)
        joined = original_list(words)
        found = [part for word in words for part in _scenes.parts_of(word)]
        if isinstance(joined, Tagged):
            joined.inside = joined.inside + found
            return joined
        return Joined(joined, found)

    stack.enter_context(mock.patch.object(Tagged, "__new__", new))
    stack.enter_context(mock.patch.object(_scenes, "parts_of", parts_of))
    stack.enter_context(mock.patch.object(i18n, "and_list", and_list))
    for name, module in list(sys.modules.items()):
        if name.startswith("telescope") and getattr(module, "and_list", None) is original_list:
            stack.enter_context(mock.patch.object(module, "and_list", and_list))


def setup_scene(id, section_id, title, width=0):
    """scene(), with texts that are put inside other texts followed."""
    def register(build):
        def run(stack):
            nested_texts(stack)
            return build(stack)
        run.__name__ = build.__name__
        return scene(id, section_id, title, width)(run)
    return register


class Variants:
    """Scenes that differ only in what happened: one scene per entry, titled and numbered from it."""

    def __init__(self, prefix, section_id, build):
        self.prefix, self.section_id, self.build, self.count = prefix, section_id, build, 0

    def add(self, slug, title, **how):
        setup_scene(f"{self.prefix}-{slug}", self.section_id, title)(lambda stack: self.build(stack, **how))


class _Shim:
    """A module (or one of its attributes) with some names swapped for fakes."""

    def __init__(self, real, **fakes):
        self._real, self._fakes = real, fakes

    def __getattr__(self, name):
        return self._fakes[name] if name in self._fakes else getattr(self._real, name)


class _Immediately:
    """A thread that runs where it's started, so a worker's whole story has happened by the time the picture is taken."""

    def __init__(self, target=None, args=(), kwargs=None, daemon=None, name=None):
        self.target, self.args, self.kwargs = target, args, kwargs or {}

    def start(self):
        self.target(*self.args, **self.kwargs)

    def is_alive(self):
        return False


class Midway(BaseException):
    """Raised by a fake to stop a worker part way, which leaves the screen as it is while that step is going on."""


def direct(target, **handlers):
    """Signals that carry text lose what a text is made of on the way, so these hand it to their slot as it is."""
    for name, handler in handlers.items():
        setattr(target, name, SimpleNamespace(emit=handler))


def direct_panel(panel):
    """The Setup panel's signals, which its workers send results through, calling the slots at once."""
    direct(panel, _sig_v4l_result=panel._on_v4l_result, _sig_v4l_unload=panel._on_v4l_unload_result,
           _sig_persist_result=panel._on_persist_result, _sig_win_checks=panel._on_win_checks,
           _sig_uc_done=panel._on_uc_done, _sig_uc_msg=panel._uc_status_lbl.setText if hasattr(panel, "_uc_status_lbl")
           else (lambda _text: None), _sig_apk_done=panel._on_apk_done, _sig_hw_found=panel._show_graphics_card)
    download = getattr(panel, "_adb_download", None)
    if download is not None:
        direct(download, progress=panel._adb_status_lbl.setText,
               finished=lambda ok, detail: (download._done(), panel._on_adb_downloaded(ok, detail)))


def immediate_threads(stack, *modules):
    for module in modules:
        stack.enter_context(mock.patch.object(module, "threading", _Shim(threading, Thread=_Immediately)))


def midway(call):
    """Run call() and let a Midway pass: the state it stopped in is the one shown."""
    try:
        call()
    except Midway:
        pass


# ── A Linux machine ──────────────────────────────────────────────────────────

CONF = ("/etc/modprobe.d/99-telescope-v4l2loopback.conf", "/etc/modules-load.d/99-telescope-v4l2loopback.conf")


def fake_linux(stack, loaded=False, nodes=(), tools=("pkexec",), answers=None, conflicts=()):
    """telescope.platform.linux on a machine where v4l2loopback is loaded or not, with these /dev and /etc files,
    these programs, and what pkexec or sudo answers as (exit code, output, error)."""
    from telescope.platform import linux
    stack.faked_linux = True
    answers = answers or {"pkexec": (0, "", "")}
    real_exists = os.path.exists

    def run(cmd, timeout=None):
        if cmd[0] == "lsmod":
            return 0, "v4l2loopback 45056 0" if loaded else "", ""
        return answers.get(cmd[0], (0, "", ""))

    def exists(path):
        return path in nodes or (not str(path).startswith(("/dev/video", "/etc/")) and real_exists(path))

    stack.enter_context(mock.patch.object(linux, "_run", run))
    stack.enter_context(mock.patch.object(
        linux, "shutil", _Shim(shutil, which=lambda name: f"/usr/bin/{name}" if name in tools else None)))
    stack.enter_context(mock.patch.object(linux, "os", _Shim(os, path=_Shim(os.path, exists=exists))))
    stack.enter_context(mock.patch.object(linux, "_find_conflicting_confs", lambda: list(conflicts)))


# ── The Setup plugin's cards ─────────────────────────────────────────────────

def advanced(stack, tab, windows=False, extras=(), apk=None, adb=None):
    """The Settings window on one of the Setup plugin's tabs, as on a Linux or a Windows computer; returns it and the
    plugin's panel. extras: the extra virtual cameras that are set up. apk: where the phone app that came with this
    copy is, if one did. adb: whether adb is found, for Windows."""
    from telescope.plugins import setup
    from telescope import vcam
    use_scratch_profile(stack)
    stack.enter_context(mock.patch.object(setup, "IS_LINUX", not windows))
    stack.enter_context(mock.patch.object(setup, "bundled_apk_path", lambda: apk))
    stack.enter_context(mock.patch.object(setup, "adb_available", lambda: adb if adb is not None else not windows))
    stack.enter_context(mock.patch.object(vcam, "slot_ready", lambda slot: slot in extras))
    immediate_threads(stack, setup)
    if not windows and not getattr(stack, "faked_linux", False):
        fake_linux(stack)
    win = main_window(stack)
    win.resize(1380, 900)
    win.open_settings()
    settings = win._settings_win
    settings.show_tab(tab)
    plugin = next(p for p in win._plugins if p.name == "setup")
    direct_panel(plugin._panel)
    return settings, plugin._panel, win


def finish(settings, width=None, height=None):
    from PyQt6.QtWidgets import QApplication
    from telescope.widgets.settings_window import _SIZE
    QApplication.processEvents()
    grow(settings, width or _SIZE[0], height or _SIZE[1])
    return settings


# ── Settings: virtual camera on Linux ────────────────────────────────────────

def kernel_module(stack, action=None, **machine):
    """The Virtual camera card on Linux, after Check, Load or Unload on a machine like this."""
    fake_linux(stack, **machine)
    settings, panel, _win = advanced(stack, "devices")
    if action:
        getattr(panel, action)()
    else:
        panel._v4l_check()
    return finish(settings)


MODULE = Variants("setup-module", "states", kernel_module)
MODULE.add("other-config", "Setup · virtual camera: module loaded with other settings", loaded=True)
MODULE.add("not-loaded", "Setup · virtual camera: module not loaded")
MODULE.add("loaded", "Setup · virtual camera: loaded", action="_v4l_load")
MODULE.add("refused", "Setup · virtual camera: already loaded with other settings", action="_v4l_load", loaded=True)
MODULE.add("not-ours", "Setup · virtual camera: the camera device is something else", action="_v4l_load",
           nodes=("/dev/video11",))
MODULE.add("no-password", "Setup · virtual camera: no password prompt, command to run", action="_v4l_load",
           tools=("pkexec", "sudo"), answers={"pkexec": (127, "", ""), "sudo": (1, "", "sudo: a password is required")})
MODULE.add("not-installed", "Setup · virtual camera: v4l2loopback not installed", action="_v4l_load",
           answers={"pkexec": (1, "", "modprobe: FATAL: Module v4l2loopback not found in directory /lib/modules")})
MODULE.add("load-failed", "Setup · virtual camera: load failed", action="_v4l_load", answers={"pkexec": (1, "", "")})
MODULE.add("unloaded", "Setup · virtual camera: unloaded", action="_v4l_unload", loaded=True)
MODULE.add("unload-failed", "Setup · virtual camera: unload failed", action="_v4l_unload", loaded=True,
           answers={"pkexec": (1, "", "")})


def persistence(stack, on, nodes=(), conflicts=(), answers=None):
    """Load when the computer starts, switched on or off on a machine with these config files."""
    fake_linux(stack, nodes=nodes, conflicts=conflicts, answers=answers)
    settings, panel, _win = advanced(stack, "devices")
    panel._persist_chk.blockSignals(True)
    panel._persist_chk.setChecked(on)
    panel._persist_chk.blockSignals(False)
    panel._on_persist_toggled(on)
    return finish(settings)


PERSIST = Variants("setup-persist", "states", persistence)
PERSIST.add("written", "Setup · load at start: files written", on=True)
PERSIST.add("already", "Setup · load at start: already set up", on=True, nodes=CONF)
PERSIST.add("conflict", "Setup · load at start: another config in the way", on=True,
            conflicts=("/etc/modprobe.d/v4l2loopback.conf",))
PERSIST.add("write-failed", "Setup · load at start: couldn't write the files", on=True, answers={"pkexec": (1, "", "")})
PERSIST.add("removed", "Setup · load at start: files removed", on=False, nodes=CONF)
PERSIST.add("nothing", "Setup · load at start: nothing to remove", on=False)
PERSIST.add("remove-failed", "Setup · load at start: couldn't remove the files", on=False, nodes=CONF,
            answers={"pkexec": (1, "", "")})


# ── Settings: canvas size ────────────────────────────────────────────────────

def canvas(stack, outcome, windows=False, **machine):
    """Video > Canvas size after Apply: still going, or how it ended."""
    from telescope.platform import linux
    fake_linux(stack, **machine)
    settings, panel, win = advanced(stack, "video", windows=windows)
    panel._canvas_apply_btn.setEnabled(True)
    win.restart_vcam_canvas = lambda w, h, on_done=None: None
    panel.set_canvas_preset("1080p 16:9 Portrait - 1080 x 1920")
    panel._apply_canvas()
    if outcome != "going":
        result = (True, "") if outcome == "reloaded" else linux.v4l2_reload()
        ok, message = result
        panel.set_canvas_apply_result(ok, message, getattr(result, "command", None) or "")
    return finish(settings)


CANVAS = Variants("setup-canvas", "states", canvas)
CANVAS.add("reloading", "Setup · canvas size: reloading the module", outcome="going")
CANVAS.add("restarting", "Setup · canvas size: restarting the stream", outcome="going", windows=True)
CANVAS.add("done", "Setup · canvas size: loopback reloaded", outcome="reloaded")
CANVAS.add("done-windows", "Setup · canvas size: canvas updated", outcome="reloaded", windows=True)
CANVAS.add("in-use", "Setup · canvas size: module still in use", outcome="busy",
           answers={"pkexec": (1, "", "modprobe: FATAL: Module v4l2loopback is in use")})
CANVAS.add("failed", "Setup · canvas size: reload failed", outcome="failed", answers={"pkexec": (1, "", "")})
CANVAS.add("no-password", "Setup · canvas size: no password prompt, command to run", outcome="nopassword",
           tools=("pkexec", "sudo"), answers={"pkexec": (127, "", ""), "sudo": (1, "", "sudo: a password is required")})


# ── Settings: extra cameras ──────────────────────────────────────────────────

def extras_removed(stack, outcome, **machine):
    """Extra cameras > Remove, on a Linux machine where taking them away goes like this."""
    from telescope import app as app_module, vcam
    fake_linux(stack, **machine)
    settings, panel, win = advanced(stack, "devices", extras=(1, 2))
    stack.enter_context(mock.patch.object(vcam, "slot_device", lambda slot: f"/dev/video{11 + slot}"))
    if outcome == "listed":
        panel._refresh_extras()
    else:
        with mock.patch.object(app_module, "threading", _Shim(threading, Thread=_Immediately)):
            panel._remove_extras()
    return finish(settings)


EXTRAS = Variants("setup-extras", "states", extras_removed)
EXTRAS.add("listed", "Setup · extra cameras: set up", outcome="listed")
EXTRAS.add("removed", "Setup · extra cameras: removed", outcome="remove")
EXTRAS.add("no-tool", "Setup · extra cameras: v4l2loopback-ctl missing", outcome="remove", tools=())
EXTRAS.add("in-use", "Setup · extra cameras: one is still open", outcome="remove", answers={"pkexec": (1, "", "")})


def adding_cameras(stack, **machine):
    """The main window after Add camera couldn't set up the extra virtual cameras, as on Linux."""
    from telescope import app as app_module
    fake_linux(stack, **machine)
    win = main_window(stack)
    win._ask_slots = lambda: True
    with mock.patch.object(app_module, "threading", _Shim(threading, Thread=_Immediately)):
        win._set_up_slots(lambda: None)
    from PyQt6.QtWidgets import QApplication
    QApplication.processEvents()
    import telescope.app as module
    grow(win, module._WIDTH_THREE_COL, 900)
    return win


ADDING = Variants("setup-add-camera", "states", adding_cameras)
ADDING.add("no-tool", "Setup · Add camera: v4l2loopback-ctl missing", tools=())
ADDING.add("failed", "Setup · Add camera: couldn't add them", answers={"pkexec": (1, "", "")})
ADDING.add("no-password", "Setup · Add camera: no password prompt, command to run", tools=("pkexec", "sudo", "v4l2loopback-ctl"),
           answers={"pkexec": (127, "", ""), "sudo": (1, "", "sudo: a password is required")})


# ── Settings: the Windows driver ─────────────────────────────────────────────

DRIVER_FILES = ("UnityCaptureFilter32.dll", "UnityCaptureFilter64.dll")


class _Download(io.BytesIO):
    pass


def fake_windows(stack, files=False, net=None, exit_code=0, registered=False, folder=None, valid=True):
    """telescope.platform.windows on a computer with the driver files there or not, a download that goes like net,
    and the registration (run as admin) ending with exit_code."""
    import subprocess
    import tempfile
    from telescope.platform import windows
    from telescope.plugins import onboarding, setup
    folder = Path(folder) if folder else Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-driver-")))
    if files:
        for name in DRIVER_FILES:
            (folder / name).write_bytes(b"driver")

    def urlopen(url, timeout=None):
        if net is None:
            raise urllib.error.URLError(TimeoutError("timed out"))
        return net(url)

    def run(cmd, **_kwargs):
        if exit_code == "timeout":
            raise subprocess.TimeoutExpired(cmd, 60)
        return SimpleNamespace(returncode=exit_code)

    for module in (windows, setup):
        stack.enter_context(mock.patch.object(module, "unitycapture_dir", lambda: folder))
    stack.enter_context(mock.patch.object(windows, "mf_installable", lambda: False))  # these scenes are UnityCapture's
    stack.enter_context(mock.patch.object(windows, "web", _Shim(windows.web, urlopen=urlopen)))
    stack.enter_context(mock.patch.object(windows, "subprocess", _Shim(subprocess, run=run)))
    if valid:
        stack.enter_context(mock.patch.object(windows, "_sha256", lambda path: windows._EXPECTED_SHA256[Path(path).name]))
    for module in (setup, onboarding):
        stack.enter_context(mock.patch.object(module, "camera_installed" if module is onboarding else "uc_registered_name",
                                              (lambda: registered) if module is onboarding else
                                              (lambda: windows.UC_NAME if registered else None)))
    return folder


def windows_driver(stack, name=None, in_app=False, files=False, adb=True, mf=""):
    """The Virtual camera card on Windows, once it has looked at what's registered. mf: the Windows 11 camera is
    "ready", an older one needs an "update", or it's on "offer" (Windows 11 with only UnityCapture)."""
    from telescope.plugins import setup
    fake_windows(stack, files=files)
    settings, panel, _win = advanced(stack, "devices", windows=True, adb=adb)
    stack.enter_context(mock.patch.object(setup, "uc_registered_name", lambda: name))
    stack.enter_context(mock.patch.object(setup, "uc_in_app_folder", lambda: in_app))
    stack.enter_context(mock.patch.object(setup, "use_mf", lambda slot=0: mf in ("ready", "update")))
    stack.enter_context(mock.patch.object(setup, "mf_outdated", lambda: mf == "update"))
    stack.enter_context(mock.patch.object(setup, "mf_installable", lambda: mf == "offer"))
    panel._check_win_setup()
    return finish(settings)


DRIVER = Variants("setup-driver", "states", windows_driver)
DRIVER.add("ready", "Setup · Windows driver: installed", name="Telescope")
DRIVER.add("reinstall", "Setup · Windows driver: reinstall for a security fix", name="Telescope", in_app=True)
DRIVER.add("renamed", "Setup · Windows driver: listed under its old name", name="Unity Video Capture")
DRIVER.add("missing", "Setup · Windows driver: not installed", files=False)
DRIVER.add("downloaded", "Setup · Windows driver: downloaded, not installed", files=True)
DRIVER.add("no-adb", "Setup · Windows driver: adb not found", name="Telescope", adb=False)
DRIVER.add("windows-11", "Setup · Windows driver: the Windows 11 camera is installed", mf="ready")
DRIVER.add("offer", "Setup · Windows driver: Windows 11 with only the older camera", name="Telescope", mf="offer")
DRIVER.add("update", "Setup · Windows driver: an older Windows 11 camera", mf="update")


def installing_driver(stack, how, files=False, net=None, exit_code=0, registered=True, valid=True):
    """Install driver on Windows, pressed: part way through, or the way it ended."""
    from telescope.plugins import setup
    from telescope.platform import windows
    fake_windows(stack, files=files, net=net, exit_code=exit_code, registered=registered, valid=valid)
    settings, panel, _win = advanced(stack, "devices", windows=True, adb=True)
    if how == "download":
        stack.enter_context(mock.patch.object(windows, "download_unitycapture", mock.Mock(side_effect=Midway())))
    elif how == "register":
        stack.enter_context(mock.patch.object(windows, "register_unitycapture", mock.Mock(side_effect=Midway())))
    midway(panel._install_uc)
    return finish(settings)


def _bytes(data):
    return lambda url: _Download(data)


INSTALL = Variants("setup-install-driver", "states", installing_driver)
INSTALL.add("downloading", "Setup · Windows driver: downloading", how="download")
INSTALL.add("registering", "Setup · Windows driver: waiting for admin access", how="register", files=True)
INSTALL.add("done", "Setup · Windows driver: installed just now", how="all", files=True)
INSTALL.add("offline", "Setup · Windows driver: download failed", how="all")
INSTALL.add("wrong-file", "Setup · Windows driver: download didn't match", how="all", net=_bytes(b"not the driver"), valid=False)
INSTALL.add("bad-copy", "Setup · Windows driver: files on disk don't match", how="all", files=True, valid=False)
INSTALL.add("denied", "Setup · Windows driver: admin access denied", how="all", files=True, exit_code=1)
INSTALL.add("changed", "Setup · Windows driver: copy in Program Files doesn't match", how="all", files=True, exit_code=2)
INSTALL.add("refused", "Setup · Windows driver: Windows refused to register it", how="all", files=True, exit_code=3)
INSTALL.add("unsafe", "Setup · Windows driver: Program Files folder isn't safe", how="all", files=True, exit_code=4)
INSTALL.add("error", "Setup · Windows driver: error while running as admin", how="all", files=True, exit_code=5)
INSTALL.add("timeout", "Setup · Windows driver: timed out", how="all", files=True, exit_code="timeout")


def no_folder(stack):
    """Install driver where the folder for the driver files can't be made."""
    fake_windows(stack, folder="/proc/telescope/unitycapture")
    settings, panel, _win = advanced(stack, "devices", windows=True, adb=True)
    panel._install_uc()
    return finish(settings)


setup_scene("setup-install-driver-no-folder", "states", "Setup · Windows driver: couldn't make its folder")(no_folder)


# ── Settings: adb on Windows ─────────────────────────────────────────────────

INDEX = """<?xml version='1.0' encoding='utf-8'?>
<sdk:sdk-repository xmlns:sdk="http://schemas.android.com/sdk/android/repo/repository2/03">
  <remotePackage path="platform-tools">
    <revision><major>37</major><minor>0</minor><micro>{micro}</micro></revision>
    <archives>
      <archive><complete><size>{size}</size><checksum type="sha1">{sha1}</checksum>
        <url>platform-tools_r37.0.1-win.zip</url></complete><host-os>windows</host-os></archive>
    </archives>
  </remotePackage>
</sdk:sdk-repository>"""
ADB_FILES = ("adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "libwinpthread-1.dll", "NOTICE.txt", "source.properties")


def platform_tools(names=ADB_FILES):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for name in names:
            archive.writestr(f"platform-tools/{name}", b"Pkg.Revision=37.0.1\n" if name == "source.properties" else name.encode())
    return buffer.getvalue()


def google(index=None, archive=None, listed=None, micro="1", fail=None, stop_at=None):
    """urlopen for dl.google.com: the index and one zip, going wrong in the way fail says (an exception raised for the
    index or the zip) or stopping where stop_at says ("index" or "zip", a Midway)."""
    archive = platform_tools() if archive is None else archive
    listed = archive if listed is None else listed
    page = (INDEX.format(micro=micro, size=len(listed), sha1=hashlib.sha1(listed).hexdigest()).encode()
            if index is None else index)
    calls = []

    def urlopen(url, timeout=None):
        which = "index" if url.endswith(".xml") else "zip"
        calls.append(which)
        if stop_at == which:
            raise Midway()
        if fail is not None and fail[0] == which:
            raise fail[1]
        return io.BytesIO(page if which == "index" else archive)

    return urlopen


def adb_download(stack, urlopen=None, source=None, pick=False, files=(), unzip=None, replace=None, version_file=True):
    """The ADB row on Windows after Get adb (or Use my own adb...): going on, or the way it ended."""
    import tempfile
    from PyQt6.QtWidgets import QFileDialog
    from telescope.platform import adb_download as downloads
    from telescope.plugins import setup
    from telescope.widgets import adb as adb_module
    base = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-adb-")))
    stack.enter_context(mock.patch.dict(os.environ, {"LOCALAPPDATA": str(base / "local")}))
    stack.enter_context(mock.patch.object(adb_module, "confirm", lambda _parent: True))
    immediate_threads(stack, adb_module)
    if unzip is not None:
        stack.enter_context(mock.patch.object(downloads, "_unzip", unzip))
    if replace is not None:
        stack.enter_context(mock.patch.object(downloads, "os", _Shim(os, replace=replace)))
    settings, panel, _win = advanced(stack, "devices", windows=True, adb=False)
    stack.enter_context(mock.patch.object(setup, "uc_registered_name", lambda: "Telescope"))
    panel._check_win_setup()
    picked = base / "picked"
    picked.mkdir()
    for name in files:
        (picked / name).write_bytes(b"Pkg.Revision=37.0.1\n" if name == "source.properties" else name.encode())
    if source == "zip":
        (picked / "tools.zip").write_bytes(b"not a zip")
    chosen = picked / (source if source not in (None, "zip") else "adb.exe")
    if source == "zip":
        chosen = picked / "tools.zip"
    stack.enter_context(mock.patch.object(QFileDialog, "getOpenFileName", staticmethod(lambda *a, **k: (str(chosen), ""))))
    if pick:
        midway(panel._pick_adb)
    else:
        panel._adb_download._download = lambda progress: downloads.download_adb(
            urlopen=urlopen or google(), dest=base / "local" / "Telescope" / "platform-tools", progress=progress)
        midway(panel._get_adb)
    return finish(settings)


ADB = Variants("setup-adb", "states", adb_download)
ADB.add("finding", "Setup · adb: finding the current version", urlopen=google(stop_at="index"))
ADB.add("downloading", "Setup · adb: downloading a version", urlopen=google(stop_at="zip"))
ADB.add("downloading-unknown", "Setup · adb: downloading, version not known", urlopen=google(micro="x", stop_at="zip"))
ADB.add("unpacking", "Setup · adb: unpacking", unzip=mock.Mock(side_effect=Midway()))
ADB.add("ready", "Setup · adb: downloaded", urlopen=google())
ADB.add("unreadable-list", "Setup · adb: Google's list isn't what was expected", urlopen=google(index=b"<html>sorry</html>"))
ADB.add("offline", "Setup · adb: can't reach Google",
        urlopen=google(fail=("index", urllib.error.URLError(ConnectionRefusedError("refused")))))
ADB.add("certificate", "Setup · adb: Google's certificate can't be checked",
        urlopen=google(fail=("index", urllib.error.URLError(__import__("ssl").SSLCertVerificationError("self-signed")))))
ADB.add("blocked", "Setup · adb: Google won't send it",
        urlopen=google(fail=("index", urllib.error.HTTPError("https://dl.google.com", 403, "Forbidden", {}, None))))
ADB.add("server-error", "Setup · adb: Google's server had a problem",
        urlopen=google(fail=("zip", urllib.error.HTTPError("https://dl.google.com", 503, "Unavailable", {}, None))))
ADB.add("download-failed", "Setup · adb: the download broke off", urlopen=google(fail=("zip", ValueError("cut off"))))
ADB.add("wrong-checksum", "Setup · adb: the download doesn't match Google's checksum",
        urlopen=google(archive=platform_tools(), listed=platform_tools()[:-1] + b"!"))
ADB.add("bad-zip", "Setup · adb: the zip can't be unpacked", urlopen=google(archive=b"not a zip file"))
ADB.add("no-adb", "Setup · adb: the zip has no adb.exe", urlopen=google(archive=platform_tools(("NOTICE.txt",))))
ADB.add("cant-place", "Setup · adb: couldn't put it in place", urlopen=google(),
        replace=mock.Mock(side_effect=OSError("in use")))
ADB.add("own", "Setup · adb: your own adb kept", pick=True, files=ADB_FILES)
ADB.add("own-unknown", "Setup · adb: your own adb kept, version not known", pick=True, files=("adb.exe", "AdbWinApi.dll"))
ADB.add("own-missing", "Setup · adb: your own pick has no adb.exe", pick=True, source="notes.txt")
ADB.add("own-unreadable", "Setup · adb: your own zip can't be read", pick=True, source="zip")


# ── Settings: the phone app over USB ─────────────────────────────────────────

def hold_pickers(stack):
    """File and choice dialogs the Setup card opens stay up for the picture and are cancelled; returns the list."""
    from PyQt6.QtWidgets import QFileDialog, QInputDialog
    import tempfile
    held = []
    empty = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-pick-"))) / "Downloads"
    empty.mkdir()

    def open_file(parent=None, caption="", directory="", name_filter="", *_args, **_kwargs):
        dialog = QFileDialog(parent, caption, str(empty), name_filter)
        dialog.setOption(QFileDialog.Option.DontUseNativeDialog, True)
        dialog.setWindowTitle(caption)
        remember(dialog, "file type", name_filter)
        held.append(dialog)
        return "", ""

    def get_item(parent, title, label, items, *_args, **_kwargs):
        dialog = QInputDialog(parent)
        dialog.setOption(QInputDialog.InputDialogOption.UseListViewForComboBoxItems, True)
        dialog.setComboBoxItems(list(items))
        dialog.setWindowTitle(title)
        dialog.setLabelText(label)
        held.append(dialog)
        return "", False

    stack.enter_context(mock.patch.object(QFileDialog, "getOpenFileName", staticmethod(open_file)))
    stack.enter_context(mock.patch.object(QInputDialog, "getItem", staticmethod(get_item)))
    return held


def phone_app(stack, windows=False, apk=None, adb=True, serials=("R58M123ABC",), answer=None, midway_install=False,
              install=True):
    """Phone app > Install over USB (or Choose APK...), pressed: what it says, or the dialog it opens. answer is what
    adb says to the install: (exit code, output, error)."""
    import tempfile
    from telescope import platform as platform_module
    from telescope.platform import adb_install
    from telescope.plugins import setup
    held = hold_pickers(stack)
    folder = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-apk-")))
    package = folder / "Telescope.apk"
    package.write_bytes(b"apk")
    settings, panel, _win = advanced(stack, "devices", windows=windows, apk=package if apk else None, adb=adb)
    stack.enter_context(mock.patch.object(setup, "run_off_ui_thread", lambda fn, *a, **k: fn(*a, **k)))
    stack.enter_context(mock.patch.object(setup, "adb_devices", lambda: list(serials)))
    stack.enter_context(mock.patch.object(platform_module, "_run", lambda cmd, timeout=None: answer or (0, "Success", "")))
    if midway_install:
        stack.enter_context(mock.patch.object(setup, "adb_install", mock.Mock(side_effect=Midway())))
    else:
        stack.enter_context(mock.patch.object(setup, "adb_install", adb_install))
    if install:
        midway(panel._install_apk)
    if held:
        return shown(stack, held[-1])
    return finish(settings)


PHONE_APP = Variants("setup-apk", "states", phone_app)
PHONE_APP.add("bundled", "Setup · phone app: one comes with this copy", apk=True, install=False)
PHONE_APP.add("no-adb-windows", "Setup · phone app: needs adb, Windows", windows=True, adb=False)
PHONE_APP.add("no-adb", "Setup · phone app: needs adb, Linux", adb=False)
PHONE_APP.add("no-phone", "Setup · phone app: no phone over USB", apk=True, serials=())
PHONE_APP.add("installing", "Setup · phone app: installing", apk=True, midway_install=True)
PHONE_APP.add("installed", "Setup · phone app: installed", apk=True)
PHONE_APP.add("signed-differently", "Setup · phone app: signed with another key", apk=True,
              answer=(1, "", "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE]"))
PHONE_APP.add("newer", "Setup · phone app: the phone has a newer one", apk=True,
              answer=(1, "", "Failure [INSTALL_FAILED_VERSION_DOWNGRADE]"))
PHONE_APP.add("install-failed", "Setup · phone app: adb install failed", apk=True, answer=(1, "", ""))
PHONE_APP.add("which-phone", "Select device question", apk=True, serials=("R58M123ABC", "emulator-5554"))
PHONE_APP.add("choose-apk", "Select APK file dialog")


# ── Settings: video, language, about and plugins ─────────────────────────────

def graphics_card(stack, device):
    """Video > Hardware acceleration once the graphics card has been looked at."""
    settings, panel, _win = advanced(stack, "video")
    panel._show_graphics_card(device)
    return finish(settings)


setup_scene("setup-graphics-card", "settings", "Settings · Video, graphics card found")(
    lambda stack: graphics_card(stack, "vaapi"))


def language(stack, chosen, draft):
    """General > Language with another language picked, or the one in use being a first draft."""
    from telescope import i18n
    if draft:
        stack.enter_context(mock.patch.object(i18n, "is_draft", lambda lang: True))
    settings, panel, _win = advanced(stack, "general")
    if chosen:  # another language than the one loaded, which is what makes it want a restart
        panel.set_language(next(lang for lang in i18n.available() if lang != i18n.language()))
    return finish(settings)


setup_scene("setup-language-restart", "settings", "Settings · General, language needs a restart")(
    lambda stack: language(stack, True, False))
setup_scene("setup-language-draft", "settings", "Settings · General, first draft translation")(
    lambda stack: language(stack, False, True))


def about(stack, copied):
    """Updates & About with the log available, and Copy diagnostics pressed or not."""
    import tempfile
    from telescope import diagnostics
    log = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-log-"))) / "telescope.log"
    log.write_text("")
    stack.enter_context(mock.patch.object(diagnostics.events, "path", log))
    settings, panel, _win = advanced(stack, "about")
    if copied:
        panel._copy_diagnostics()
    return finish(settings)


setup_scene("setup-about", "settings", "Settings · Updates & About, bug report buttons")(lambda stack: about(stack, False))
setup_scene("setup-about-copied", "settings", "Settings · Updates & About, diagnostics copied")(
    lambda stack: about(stack, True))


def plugins_page(stack, off):
    """Plugins with the built-in ones listed, as when Telescope was started with some of them switched off."""
    from telescope import plugin_loader
    from telescope.plugins import builtin_plugins
    from PyQt6.QtWidgets import QApplication
    from telescope.widgets.settings_window import _SIZE
    use_scratch_profile(stack)
    win = main_window(stack)
    for name in off:
        plugin_loader.set_builtin_off(name, True)
    win._builtin_plugins = builtin_plugins()
    # Optional plugins without the vision download start out off, as they do in the app
    win._plugins = [p for p in win._plugins if p.name not in off and not (p.optional and not p.needs_vision)]
    win.resize(1380, 900)
    win.open_settings()
    settings = win._settings_win
    settings.show_tab("plugins")
    QApplication.processEvents()
    grow(settings, _SIZE[0], 1500)
    return settings


setup_scene("setup-plugins-off", "settings", "Settings · Plugins, Setup guide switched off")(
    lambda stack: plugins_page(stack, ("onboarding",)))


# ── Main window: the setup checklist ─────────────────────────────────────────

def checklist(stack, windows=False, apk=False, adb=True, phone=False, act=None, serials=("R58M123ABC",), answer=None,
              adb_install_midway=False, register_midway=False, **driver):
    """The main window with the Get set up checklist, after act(plugin, bus) has put it in a state. apk and adb: whether
    the phone app came with this copy and adb is found. driver: how the Windows driver goes (see fake_windows)."""
    import tempfile
    import telescope.app as app_module
    from telescope import platform as platform_module
    from telescope.platform import adb_install
    from telescope.plugins import onboarding
    package = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-apk-"))) / "Telescope.apk"
    package.write_bytes(b"apk")
    if windows:
        fake_windows(stack, **driver)
    else:
        fake_linux(stack)
    stack.enter_context(mock.patch.object(onboarding, "IS_LINUX", not windows))
    stack.enter_context(mock.patch.object(onboarding, "bundled_apk_path", lambda: package if apk else None))
    stack.enter_context(mock.patch.object(onboarding, "adb_available", lambda: adb))
    stack.enter_context(mock.patch.object(onboarding, "adb_devices", lambda: list(serials)))
    stack.enter_context(mock.patch.object(platform_module, "_run", lambda cmd, timeout=None: answer or (0, "Success", "")))
    if adb_install_midway:
        stack.enter_context(mock.patch.object(onboarding, "adb_install", mock.Mock(side_effect=Midway())))
    else:
        stack.enter_context(mock.patch.object(onboarding, "adb_install", adb_install))
    if register_midway:
        from telescope.platform import windows as windows_module
        stack.enter_context(mock.patch.object(windows_module, "register_unitycapture", mock.Mock(side_effect=Midway())))
    immediate_threads(stack, onboarding)
    win = main_window(stack, phone=phone)
    plugin = next(p for p in win._plugins if p.name == "onboarding")
    plugin._signals = SimpleNamespace(vcam=SimpleNamespace(emit=plugin._on_vcam), apk=SimpleNamespace(emit=plugin._on_apk))
    if act is not None:
        midway(lambda: act(plugin, win._bus))
    grow(win, app_module._WIDTH_THREE_COL, 900)
    return win


CHECKLIST = Variants("checklist", "main", checklist)
CHECKLIST.add("linux-missing", "Setup guide · virtual camera package missing", act=lambda p, b: p._on_vcam(False, ""))
CHECKLIST.add("linux-installed", "Setup guide · virtual camera installed", act=lambda p, b: p._on_vcam(True, ""))
CHECKLIST.add("windows-missing", "Setup guide · Windows driver missing", windows=True, act=lambda p, b: p._on_vcam(False, ""))
CHECKLIST.add("windows-installing", "Setup guide · Windows driver installing", windows=True, files=True,
              register_midway=True, act=lambda p, b: (p._on_vcam(False, ""), p._vcam_action()))
CHECKLIST.add("windows-installed", "Setup guide · Windows driver installed", windows=True, files=True, registered=True,
              act=lambda p, b: (p._on_vcam(False, ""), p._vcam_action()))
CHECKLIST.add("windows-offline", "Setup guide · Windows driver download failed", windows=True,
              act=lambda p, b: (p._on_vcam(False, ""), p._vcam_action()))
CHECKLIST.add("windows-denied", "Setup guide · Windows driver, admin access denied", windows=True, files=True, exit_code=1,
              act=lambda p, b: (p._on_vcam(False, ""), p._vcam_action()))
CHECKLIST.add("usb-offered", "Setup guide · phone app can be installed over USB", apk=True, act=lambda p, b: p._render())
CHECKLIST.add("usb-installing", "Setup guide · phone app installing over USB", apk=True, adb_install_midway=True,
              act=lambda p, b: p._install_apk())
CHECKLIST.add("usb-installed", "Setup guide · phone app installed over USB", apk=True, act=lambda p, b: p._install_apk())
CHECKLIST.add("usb-no-phone", "Setup guide · no phone found over USB", apk=True, serials=(),
              act=lambda p, b: p._install_apk())
CHECKLIST.add("usb-signed-differently", "Setup guide · phone app signed with another key", apk=True,
              answer=(1, "", "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE]"), act=lambda p, b: p._install_apk())
CHECKLIST.add("not-a-phone", "Setup guide · a camera that isn't a phone is picked",
              act=lambda p, b: (p._on_vcam(True, ""), b.source_selected.emit("camera-elsewhere")))


def guide_again(stack):
    """The checklist opened again from the help button, with everything set up and the first stream done."""
    def act(plugin, bus):
        plugin.set_config({"streamed": True})
        plugin._on_vcam(True, "")
        bus.phones_changed.emit(1)
        bus.setup_guide_requested.emit()
    return checklist(stack, phone=True, act=act)


setup_scene("checklist-opened-again", "main", "Setup guide · opened again once everything works")(guide_again)


# ── Dialogs: Set up VA-API ───────────────────────────────────────────────────

class _Pip:
    """What pip does when the build runs: prints these lines and ends with this exit code."""

    def __init__(self, lines, code):
        self.stdout, self.returncode = iter(lines), code

    def wait(self):
        pass


def fake_build_machine(stack, tools=("cc", "pkg-config"), ffmpeg=True, headers=True, distro=("fedora",), pip=None,
                       probe=None, standard="18.1.0"):
    """telescope.pyav_vaapi on a computer with these build tools, this distro and a PyAV of this version, where pip
    ends like pip (lines, exit code) and a build is checked like probe (ok, version, why not)."""
    import subprocess
    import sysconfig
    import tempfile
    from telescope import pyav_vaapi
    data = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-vaapi-")))
    include = data / "include"
    include.mkdir()
    if headers:
        (include / "Python.h").write_text("")
    stack.enter_context(mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(data)}))
    stack.enter_context(mock.patch.object(pyav_vaapi, "standard_version", lambda: standard))
    stack.enter_context(mock.patch.object(pyav_vaapi, "find_system", lambda: None))
    stack.enter_context(mock.patch.object(pyav_vaapi, "_distro", lambda: set(distro)))
    stack.enter_context(mock.patch.object(pyav_vaapi, "probe", lambda folder: probe or (True, standard, "")))
    stack.enter_context(mock.patch.object(pyav_vaapi, "active", None))
    stack.enter_context(mock.patch.object(pyav_vaapi, "problem", ""))
    stack.enter_context(mock.patch.object(
        pyav_vaapi, "shutil", _Shim(shutil, which=lambda name: f"/usr/bin/{name}" if name in tools else None)))
    stack.enter_context(mock.patch.object(pyav_vaapi, "sysconfig", _Shim(sysconfig, get_paths=lambda: {"include": str(include)})))
    stack.enter_context(mock.patch.object(pyav_vaapi, "subprocess", _Shim(
        subprocess, run=lambda cmd, **k: SimpleNamespace(returncode=0 if ffmpeg else 1),
        Popen=lambda cmd, **k: _Pip(*(pip or (["Building wheel for av"], 0))))))
    return data / "Telescope" / "pyav-vaapi"


def vaapi_dialog(stack, system=None, built=None, running=False, picked=None, active=None, problem="", restart=False,
                 build=False, **machine):
    """Set up VA-API with what the computer has: system is what the distro's PyAV checked out as (folder, ok, version,
    why not), built whether an existing build works (None for no build), running a build having been started, picked
    one of them, active what this run uses, problem why the one picked isn't."""
    from telescope import pyav_vaapi
    from telescope.widgets import vaapi_setup
    use_scratch_profile(stack)
    root = fake_build_machine(stack, **machine)
    version = pyav_vaapi.standard_version()
    if built is not None:
        (root / version / "av").mkdir(parents=True)
    if picked:
        pyav_vaapi.choose(picked[0], root / picked[1])
    if active:
        stack.enter_context(mock.patch.object(pyav_vaapi, "active", {**active, "path": str(root / active["path"])}))
    if problem:
        stack.enter_context(mock.patch.object(pyav_vaapi, "problem", problem))
    stack.enter_context(mock.patch.object(vaapi_setup.VaapiSetupDialog, "check_again", lambda self: None))
    dialog = vaapi_setup.VaapiSetupDialog(None, (lambda: True) if restart else None)
    direct(dialog, _sig_built=dialog._on_built, _sig_line=dialog._on_line)
    dialog._on_system(system if system is None else (root / system[0], *system[1:]))
    dialog._on_existing(bool(built))
    dialog._checks = 0
    if running:
        immediate = _Shim(threading, Thread=lambda **k: SimpleNamespace(start=lambda: None))
        stack.enter_context(mock.patch.object(vaapi_setup, "threading", immediate))
        dialog._start_build()
    elif build:
        immediate_threads(stack, vaapi_setup)
        dialog._start_build()
    return shown(stack, dialog)


def problem_after_start(stack, why, built_for="", version="18.1.0"):
    """Why a picked PyAV isn't in use, as the start-up check worked it out and Set up VA-API says it."""
    from telescope import pyav_vaapi
    root = fake_build_machine(stack, probe=(False, version, why))
    pyav_vaapi.choose(pyav_vaapi.BUILD, root / (built_for or pyav_vaapi.standard_version()))
    stack.enter_context(mock.patch.object(pyav_vaapi, "supported", lambda: True))
    pyav_vaapi.activate()
    return vaapi_dialog(stack, problem=pyav_vaapi.problem, picked=("build", built_for or pyav_vaapi.standard_version()))


VAAPI = Variants("vaapi", "dialogs", vaapi_dialog)
VAAPI.add("can-build", "Set up VA-API · nothing there yet, can build")
VAAPI.add("distro-ok", "Set up VA-API · the distro's PyAV works", system=("system", True, "19.0.1", ""))
VAAPI.add("distro-old", "Set up VA-API · the distro's PyAV is too old", system=("system", False, "11.0.0", "old"))
VAAPI.add("distro-no-vaapi", "Set up VA-API · the distro's PyAV has no VA-API", system=("system", False, "19.0.1", "novaapi"))
VAAPI.add("distro-no-open", "Set up VA-API · VA-API doesn't open with the distro's PyAV",
          system=("system", False, "19.0.1", "noopen"))
VAAPI.add("distro-no-load", "Set up VA-API · the distro's PyAV doesn't load", system=("system", False, "", "load"))
VAAPI.add("tools-missing", "Set up VA-API · build tools missing, command to install them", tools=(), ffmpeg=False, headers=False)
VAAPI.add("tools-unknown-distro", "Set up VA-API · build tools missing, distro not known", tools=("cc",), ffmpeg=False,
          distro=())
VAAPI.add("pkg-config-missing", "Set up VA-API · pkg-config missing", tools=("cc",), distro=("debian",))
VAAPI.add("ffmpeg-missing", "Set up VA-API · FFmpeg's development files missing", tools=("cc", "pkg-config"), ffmpeg=False,
          headers=False)
VAAPI.add("old-pyav", "Set up VA-API · Telescope's PyAV is too old to build against", standard="13.1.0")
VAAPI.add("built", "Set up VA-API · a build is there", built=True)
VAAPI.add("built-broken", "Set up VA-API · the existing build stopped working", built=False)
VAAPI.add("starting", "Set up VA-API · starting the build", running=True)
VAAPI.add("build-no-vaapi", "Set up VA-API · built, but VA-API doesn't open", build=True, probe=(False, "", "novaapi"))
VAAPI.add("build-no-load", "Set up VA-API · built, but it doesn't load", build=True, probe=(False, "", "load"))
VAAPI.add("build-failed", "Set up VA-API · the build failed", build=True,
          pip=(["error: libavcodec/avcodec.h: No such file or directory"], 1))
VAAPI.add("picked", "Set up VA-API · picked, restart to switch", system=("system", True, "19.0.1", ""),
          picked=("system", "system"), restart=True)
VAAPI.add("using-distro", "Set up VA-API · using the distro's PyAV", system=("system", True, "19.0.1", ""),
          picked=("system", "system"), active={"kind": "system", "version": "19.0.1", "path": "system"})
VAAPI.add("using-built", "Set up VA-API · using the built PyAV", built=True, picked=("build", "18.1.0"),
          active={"kind": "build", "version": "18.1.0", "path": "18.1.0"})

PROBLEMS = (("other-version", "Set up VA-API · picked build is for another PyAV", "", "17.0.0"),
            ("old", "Set up VA-API · picked PyAV is older than 14", "old", ""),
            ("no-vaapi", "Set up VA-API · picked PyAV has no VA-API", "novaapi", ""),
            ("no-open", "Set up VA-API · VA-API didn't open with the picked PyAV", "noopen", ""),
            ("no-load", "Set up VA-API · the picked PyAV didn't load", "load", ""))
for _slug, _title, _why, _built_for in PROBLEMS:
    setup_scene(f"vaapi-not-in-use-{_slug}", "dialogs", _title)(
        lambda stack, why=_why, built_for=_built_for: problem_after_start(stack, why, built_for))


# ── Dialogs: a second Telescope ──────────────────────────────────────────────

def lock_held(stack, inet):
    """Telescope can't start, as on a computer where the lock is a port (Windows) or a local socket (Linux)."""
    import socket
    from telescope import single_instance
    held = hold_dialogs(stack)
    stack.enter_context(mock.patch.object(single_instance, "_INSTANCE_FAMILY", socket.AF_INET if inet else socket.AF_UNIX))
    stack.enter_context(mock.patch.object(single_instance, "_INSTANCE_ADDRESS", ("127.0.0.1", 47653)))
    single_instance._port_taken_notice()
    return shown(stack, held[-1])


setup_scene("dialog-lock-held-port", "dialogs", "Telescope can't start · lock held on a port")(
    lambda stack: lock_held(stack, True))
setup_scene("dialog-lock-held-socket", "dialogs", "Telescope can't start · lock held on a local socket")(
    lambda stack: lock_held(stack, False))


# ── States: the microphone's virtual device ──────────────────────────────────

def microphone_card(stack, phone=True):
    """The main window with the Phone mic switched on, and the mic plugin's setup, install and signals working at once."""
    import telescope.app as app_module
    win = main_window(stack, phone=phone)
    plugin = next(p for p in win._plugins if p.name == "microphone")
    plugin._run_job = lambda fn, *args: fn(*args)
    plugin._sig = SimpleNamespace(
        status=SimpleNamespace(emit=plugin._on_worker_status), prepared=SimpleNamespace(emit=plugin._on_prepared),
        installed=SimpleNamespace(emit=plugin._on_installed))
    plugin._toggle.setChecked(True)
    return win, plugin


def finish_window(win):
    import telescope.app as app_module
    grow(win, app_module._WIDTH_THREE_COL, 900)
    return win


def sound_server(stack, how):
    """Phone mic on Linux, when setting up its virtual microphone goes this way."""
    from telescope import audio  # noqa: F401
    from telescope.platform import virtual_mic
    from telescope.plugins.microphone import LinuxMic

    def run(cmd, **_kwargs):
        if cmd[:3] == ["pactl", "list", "short"]:
            return (False, "") if how == "silent" else (True, "")
        return False, "Failure: Module initialization failed"

    class Backend(LinuxMic):
        def prepare(self):
            self._modules, err = virtual_mic.linux_setup(run=run, fifo="/run/user/1000/telescope-mic.fifo")
            return err

    stack.enter_context(mock.patch.object(virtual_mic, "linux_tools_missing", lambda which=None: []))
    if how == "folder":
        def refuse():
            raise OSError("/tmp/telescope-1000 isn't a private folder of this user's, so the microphone won't use it")
        stack.enter_context(mock.patch.object(virtual_mic, "fifo_path", refuse))
    win, plugin = microphone_card(stack)
    plugin._backend = Backend()
    plugin._ctrl = SimpleNamespace(base="http://phone:8080", auth="token")
    plugin._start_worker()
    return finish_window(win)


SOUND = Variants("mic-setup", "states", sound_server)
SOUND.add("silent", "Microphone · the sound server didn't answer", how="silent")
SOUND.add("refused", "Microphone · the virtual microphone couldn't be made", how="refused")
SOUND.add("folder", "Microphone · no private folder for the virtual microphone", how="folder")


def vb_cable(stack, how):
    """Phone mic on Windows after Install VB-Cable, when the download or its setup goes this way."""
    import tempfile
    from telescope.platform import virtual_mic
    from telescope.plugins.microphone import WindowsMic
    pack = io.BytesIO()
    with zipfile.ZipFile(pack, "w") as archive:
        archive.writestr(virtual_mic.VB_CABLE_SETUP if how != "no-setup" else "readme.txt", b"setup")
    data = b"not a zip" if how == "unreadable" else pack.getvalue() if how != "changed" else b"something else"
    folder = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-vbcable-")))

    def urlopen(url, timeout=None):
        if how == "refused":
            raise urllib.error.HTTPError(url, 403, "Forbidden", {}, None)
        if how == "offline":
            raise urllib.error.URLError(ConnectionRefusedError("refused"))
        if how == "certificate":
            raise urllib.error.URLError(__import__("ssl").SSLCertVerificationError("self-signed"))
        if how == "broken":
            raise ValueError("cut off")
        return io.BytesIO(data)

    def start(path, verb, cwd=None):
        if how in ("denied", "cant-open"):
            error = OSError("nope")
            if how == "denied":
                error.winerror = 1223
            raise error

    if how not in ("changed", "setup-changed"):
        stack.enter_context(mock.patch.object(virtual_mic, "VB_CABLE_PACK_SHA256", hashlib.sha256(data).hexdigest()))
    if how not in ("setup-changed",):
        stack.enter_context(mock.patch.object(virtual_mic, "VB_CABLE_SETUP_SHA256", hashlib.sha256(b"setup").hexdigest()))

    class Backend(WindowsMic):
        def problem(self):
            return None

        def install(self):
            setup, err = virtual_mic.download_vb_cable(folder=folder, urlopen=urlopen)
            if not err:
                err = virtual_mic.run_vb_cable_setup(setup, start=start)
            self.setup_opened = not err
            return err

    win, plugin = microphone_card(stack)
    plugin._backend = Backend()
    plugin._confirm_install = lambda: True
    plugin._install()
    return finish_window(win)


VB = Variants("mic-vb-cable", "states", vb_cable)
VB.add("refused", "Microphone · VB-Audio's site said no", how="refused")
VB.add("offline", "Microphone · can't reach vb-audio.com", how="offline")
VB.add("certificate", "Microphone · vb-audio.com's certificate can't be checked", how="certificate")
VB.add("broken", "Microphone · the VB-Cable download broke off", how="broken")
VB.add("changed", "Microphone · VB-Audio's download has changed", how="changed")
VB.add("unreadable", "Microphone · the VB-Cable download can't be unpacked", how="unreadable")
VB.add("no-setup", "Microphone · VB-Audio's pack has no setup", how="no-setup")
VB.add("setup-changed", "Microphone · VB-Cable's setup changed after the check", how="setup-changed")
VB.add("denied", "Microphone · Windows didn't allow the VB-Cable setup", how="denied")
VB.add("cant-open", "Microphone · VB-Cable's setup wouldn't open", how="cant-open")


# ── States: global shortcuts ─────────────────────────────────────────────────

BINDINGS = (("Ctrl+Alt+M", "microphone.mute"), ("Ctrl+T", "camera.torch"), ("Ctrl+/", "window.show_hide"),
            ("F9", "streaming.start_stop"))


def shortcuts_page(stack, backend, windows=False):
    """Keyboard shortcuts with four everywhere shortcuts and the system's backend telling how each one went."""
    from PyQt6.QtWidgets import QApplication
    from telescope.plugins import shortcuts
    from telescope.shortcuts import new_binding_id
    from telescope.widgets.settings_window import _SIZE
    use_scratch_profile(stack)
    stack.enter_context(mock.patch.object(shortcuts, "IS_LINUX", not windows))
    win = main_window(stack)
    plugin = next(p for p in win._plugins if p.name == "shortcuts")
    plugin._backend = backend(stack)
    plugin._backend.changed.connect(plugin._refresh_section)
    plugin.set_bindings([{"id": new_binding_id(), "keys": keys, "action": action, "params": {}, "global": True}
                         for keys, action in BINDINGS])
    win.resize(1380, 900)
    win.open_settings()
    settings = win._settings_win
    settings.show_tab("shortcuts")
    QApplication.processEvents()
    grow(settings, _SIZE[0], 900)
    return settings


def on_windows(stack, taken=("Ctrl+T",)):
    from PyQt6.QtCore import QCoreApplication  # noqa: F401
    from telescope.platform.hotkeys_windows import MOD_CONTROL, WindowsHotkeys

    class User32:
        registered = {}

        def RegisterHotKey(self, hwnd, number, flags, vk):
            return 0 if vk == ord("T") else 1

        def UnregisterHotKey(self, hwnd, number):
            return 1

        def GetAsyncKeyState(self, vk):
            return 0

        def VkKeyScanW(self, ch):
            return -1

    backend = WindowsHotkeys(user32=User32(), install_filter=False)
    stack.callback(backend.stop)
    return backend


def on_portal(stack, state, why="", props=None):
    """The portal's backend, on a desktop where it's in this state, without talking to any desktop."""
    from telescope.platform import hotkeys_portal
    stack.enter_context(mock.patch.object(hotkeys_portal, "threading", _Shim(
        threading, Thread=lambda **k: SimpleNamespace(start=lambda: None, is_alive=lambda: False))))
    backend = hotkeys_portal.PortalHotkeys("telescope")
    backend._state, backend._why = state, why
    plugin_ids = {}
    backend._statuses = plugin_ids
    return backend


def portal_ready(stack):
    """The portal after asking: one shortcut confirmed with the key the desktop settled on, one it wasn't shown for, and
    one with no key set."""
    from telescope.platform import hotkeys_portal
    from telescope.plugins import shortcuts  # noqa: F401
    backend = on_portal(stack, "ready")
    backend.apply = lambda hotkeys: setattr(backend, "_statuses", {
        hotkeys[0].id: hotkeys_portal.PortalHotkeys._status_of({"trigger_description": ("s", "Ctrl+Alt+M")}),
        hotkeys[1].id: hotkeys_portal.PortalHotkeys._status_of(None),
        hotkeys[2].id: hotkeys_portal.PortalHotkeys._status_of({"trigger_description": ("s", "")})})
    return backend


setup_scene("shortcuts-windows", "states", "Keyboard shortcuts · Windows, a key another app took", )(
    lambda stack: shortcuts_page(stack, on_windows, windows=True))
setup_scene("shortcuts-portal-unavailable", "states", "Keyboard shortcuts · the desktop offers no global shortcuts")(
    lambda stack: shortcuts_page(stack, lambda s: on_portal(s, "unavailable", "no portal found")))
setup_scene("shortcuts-portal-unavailable-plain", "states", "Keyboard shortcuts · no global shortcuts, no reason given")(
    lambda stack: shortcuts_page(stack, lambda s: on_portal(s, "unavailable")))
setup_scene("shortcuts-portal-asking", "states", "Keyboard shortcuts · waiting for the desktop's dialog")(
    lambda stack: shortcuts_page(stack, lambda s: on_portal(s, "asking")))
setup_scene("shortcuts-portal-ready", "states", "Keyboard shortcuts · the desktop settled on keys")(
    lambda stack: shortcuts_page(stack, portal_ready))


# ── States: signing in, the vision download, the wait screen ─────────────────

def sign_in(stack, turn_on):
    """The main window after switching Open Telescope when I sign in on or off failed, as it does where the folder
    for the entry can't be written."""
    from telescope.platform import autostart

    def refuse(*_args, **_kwargs):
        raise PermissionError(13, "Permission denied", "/home/me/.config/autostart/telescope.desktop")

    entry = SimpleNamespace(parent=SimpleNamespace(mkdir=refuse), unlink=refuse)
    stack.enter_context(mock.patch.object(autostart, "desktop_file", lambda config_home=None: entry))
    win = main_window(stack)
    plugin = next(p for p in win._plugins if p.name == "startup")
    plugin.set_open_at_sign_in(turn_on)
    return finish_window(win)


setup_scene("sign-in-on-failed", "states", "Startup · couldn't set Telescope to open at sign-in")(
    lambda stack: sign_in(stack, True))
setup_scene("sign-in-off-failed", "states", "Startup · couldn't stop Telescope opening at sign-in")(
    lambda stack: sign_in(stack, False))


def vision(stack, state, running=False, percent=None, available=True):
    """The Background card switched to Blur while the vision download is in this situation."""
    from telescope.widgets import vision_download
    win = main_window(stack)
    plugin = next(p for p in win._plugins if p.name == "background")
    shared = vision_download.shared()
    stack.enter_context(mock.patch.object(shared, "available", lambda: available, create=True))
    stack.enter_context(mock.patch.object(shared, "size_mb", lambda: 98, create=True))
    stack.enter_context(mock.patch.object(shared, "running", lambda: running, create=True))
    stack.enter_context(mock.patch.object(win._vision, "state", lambda: state))
    plugin._set_mode("blur")
    win._bus.vision_state_changed.emit(state)
    if percent is not None:
        shared.progress.emit(percent, 100)
    return finish_window(win)


VISION = Variants("vision", "states", vision)
VISION.add("missing", "Background · needs a one-time download", state="missing")
VISION.add("downloading", "Background · downloading", state="missing", running=True)
VISION.add("downloading-percent", "Background · downloading, part way", state="missing", running=True, percent=42)
VISION.add("loading", "Background · getting ready", state="loading")
VISION.add("failed", "Background · MediaPipe didn't load", state="failed")


def pack_download(stack, how):
    """Plugins, with the vision download for Auto framing ended the way vision_pack says it did."""
    import tempfile
    from telescope import vision_pack
    from telescope.widgets import vision_download
    entry = {"id": "ab" * 32, "size": 100, "files": []}
    folder = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-pack-")))
    archive = io.BytesIO()
    with zipfile.ZipFile(archive, "w") as pack:
        pack.writestr("../outside.txt" if how == "paths" else "model.task", b"model")
    data = b"not a zip" if how == "unreadable" else archive.getvalue() if how != "big" else b"x" * 1000
    if how == "no-pack":
        entry = None
    stack.enter_context(mock.patch.object(vision_pack, "expected", lambda info=None: entry))
    stack.enter_context(mock.patch.object(vision_pack, "asset_name", lambda: None if entry is None else "pack.zip"))
    stack.enter_context(mock.patch.object(vision_pack, "pack_id", lambda: None if entry is None else "ab" * 8))

    def opener(url, timeout):
        if how == "offline":
            raise OSError("offline")
        return io.BytesIO(data)

    cancelled = (lambda: True) if how == "cancelled" else (lambda: False)
    try:
        vision_pack.download(folder, cancelled=cancelled, opener=opener)
    except vision_pack.PackError as error:
        message = str(error)
    else:
        message = ""
    shared = vision_download.shared()
    shared.error, shared.failed_for = message, "framing"
    return plugins_page(stack, ())


PACK = Variants("vision-pack", "states", pack_download)
PACK.add("no-pack", "Plugins · no download for this computer", how="no-pack")
PACK.add("offline", "Plugins · the download failed", how="offline")
PACK.add("cancelled", "Plugins · the download was cancelled", how="cancelled")
PACK.add("big", "Plugins · the download is bigger than it should be", how="big")
PACK.add("unreadable", "Plugins · the download can't be unpacked", how="unreadable")
PACK.add("paths", "Plugins · the download has unexpected paths", how="paths")
PACK.add("wrong", "Plugins · the download isn't what this version expects", how="wrong")


def waiting_for_camera(stack):
    """What other apps see on Telescope's virtual camera while nothing streams."""
    from PyQt6.QtCore import Qt
    from PyQt6.QtGui import QImage, QPainter, QPixmap
    from PyQt6.QtWidgets import QApplication, QLabel, QVBoxLayout, QWidget
    from telescope import vcam
    drawn = []
    draw = QPainter.drawText

    def drawing(painter, *args):
        drawn.extend(a for a in args if isinstance(a, Tagged))
        return draw(painter, *args)

    stack.enter_context(mock.patch.object(QPainter, "drawText", drawing))
    frame = vcam._default_screen(960, 540)
    image = QImage(frame.data, 960, 540, 960 * 3, QImage.Format.Format_RGB888).copy()
    top = QWidget()
    lay = QVBoxLayout(top)
    lay.setContentsMargins(12, 12, 12, 12)
    label = QLabel()
    label.setPixmap(QPixmap.fromImage(image).scaled(640, 360, Qt.AspectRatioMode.KeepAspectRatio,
                                                    Qt.TransformationMode.SmoothTransformation))
    lay.addWidget(label)
    remember(label, "text", drawn[0])
    top.show()
    QApplication.processEvents()
    stack.callback(top.close)
    return top


setup_scene("vcam-waiting", "states", "Virtual camera · waiting for a stream")(waiting_for_camera)
