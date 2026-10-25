"""Scenes for updates, startup and monitoring: see translation_scenes.py for how scenes are registered.

The Updates page runs the real plugin and the real download and install code. Only what leaves the process is replaced:
the web, the folder it installs into, the installer and the thread that would do the work.
"""

import hashlib
import io
import json
import ssl
import subprocess
import tarfile
import tempfile
import threading
import time
import urllib.error
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from scripts.translation_scenes import (Joined, bubble, capture_notifications, grow, main_window, parts_of, remember,
                                        scene, shown)


class HostWith:
    """The plugin's host, with a few answers replaced: a stream that's running, a phone behind the shown one."""

    def __init__(self, real, **replaced):
        self._real = real
        self._replaced = replaced

    def __getattr__(self, name):
        if name in self._replaced:
            return self._replaced[name]
        return getattr(self._real, name)


def plugin_of(win, name):
    return next(p for p in win._plugins if p.name == name)


def settings_of(stack, win, tab):
    """The Settings window of win on one page, as tall as its content needs."""
    from PyQt6.QtWidgets import QApplication
    from telescope.widgets.settings_window import _SIZE
    win.resize(1380, 900)
    win.open_settings()
    settings = win._settings_win
    settings.show_tab(tab)
    QApplication.processEvents()
    grow(settings, _SIZE[0], _SIZE[1])
    return settings


# ── Updates ──────────────────────────────────────────────────────────────────

RELEASE = "4.2.0"
SETUP = "TelescopeSetup.exe"


def tar_of(entries, symlink=None):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        for name, data in entries.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
        if symlink:
            info = tarfile.TarInfo(symlink)
            info.type = tarfile.SYMTYPE
            info.linkname = "/etc/passwd"
            archive.addfile(info)
    return buffer.getvalue()


def zip_of(entries):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
    return buffer.getvalue()


class Reply(io.BytesIO):
    """What the web gives back: the body, read in chunks, with a hook that runs before each one."""

    def __init__(self, body, before=None):
        super().__init__(body)
        self._before = before

    def read(self, size=-1):
        if self._before:
            self._before()
        return super().read(size)


class World:
    """Telescope's update code with no network, no real install folder and no installer.

    The release the web offers is set with offer(). The patches last as long as the scene's stack.
    """

    def __init__(self, stack, channel="stable", build=100, app_channel=None, windows=False, unzipped=False,
                 writable=True, streaming=False, packaged=True):
        import update_guard
        import telescope.plugins.updates as plugin_module
        from telescope import updates, version
        self.updates = updates
        self.channel, self.streaming = channel, streaming
        folder = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-translation-update-")))
        self.install_folder = folder / "app"
        if writable:
            self.install_folder.mkdir()
        self.failed = 0
        self.routes = {}
        self.plugin = None
        self.on_read = None
        self.installer = SimpleNamespace(result=0, raises=None, location=None)
        self.relaunch_fails = False
        real_setup = updates.install_with_setup

        def install_with_setup(setup, old_dir=None):
            def run(argv, timeout=None, **_kwargs):
                if self.installer.raises:
                    raise self.installer.raises
                return SimpleNamespace(returncode=self.installer.result)

            return real_setup(setup, old_dir, run=run, locate=lambda: self.installer.location, folders=[])

        def run_now(target, daemon=None):
            return SimpleNamespace(start=target)

        def relaunch(argv, popen=None):
            if self.relaunch_fails:
                raise OSError("no")

        for target, name, value in (
            (version, "CHANNEL", app_channel or channel),
            (version, "BUILD", build),
            (updates, "IS_WINDOWS", windows),
            (updates, "install_dir", lambda: self.install_folder),
            (updates, "is_unzipped_copy", lambda directory=None: unzipped),
            (updates, "install_with_setup", install_with_setup),
            *(((updates, "self_update_blocker", lambda directory=None: None),) if windows and packaged else ()),  # a frozen exe
            (update_guard, "failed_build", lambda directory=None: self.failed),
            (updates.web, "urlopen", self.urlopen),
            (updates.UpdateError, "__str__", lambda error: error.args[0]),  # keeps the tag the message carries
            (plugin_module, "stop_adb_server", lambda: None),
            (plugin_module, "threading", SimpleNamespace(Event=threading.Event, Thread=run_now)),
            (plugin_module.UpdatesPlugin, "_relaunch", staticmethod(relaunch)),
        ):
            stack.enter_context(mock.patch.object(target, name, value))

    def urlopen(self, request, _timeout=None):
        url = request.full_url
        for part, answer in self.routes.items():
            if part in url:
                if isinstance(answer, BaseException):
                    raise answer
                return Reply(answer[0], None if "manifest.json" in url else self.on_read)
        raise urllib.error.URLError(OSError("offline"))

    def offer(self, build=200, assets=None, notes="https://example.com/notes"):
        """The channel's manifest. assets: {name: (body, size or None, sha256 or None)}, the usual ones when None."""
        listed = []
        for name, (body, size, sha) in (self.assets() if assets is None else assets).items():
            url = f"https://example.com/download/{name}"
            listed.append({"name": name, "url": url, "sha256": sha or hashlib.sha256(body).hexdigest(),
                           "size": len(body) if size is None else size})
            self.routes[url] = (body,)
        manifest = {"version": RELEASE, "build": build, "channel": self.channel, "versionName": RELEASE, "notes": notes,
                    "protocol": 1, "assets": listed}
        self.routes["manifest.json"] = (json.dumps(manifest).encode(),)

    def assets(self, linux=None, windows=None):
        """The files of a release: what Linux and Windows download, and the installer an unzipped copy uses."""
        linux = tar_of({"main.py": b"", "telescope/__init__.py": b""}) if linux is None else linux
        windows = zip_of({"TelescopeDesktop.exe": b"MZ"}) if windows is None else windows
        return {self.updates.LINUX_ASSET: (linux, None, None), self.updates.WINDOWS_ASSET: (windows, None, None),
                SETUP: (b"MZ", None, None)}

    def window(self, stack):
        """The main window with the Updates plugin wired to the fake web, thread and host."""
        win = main_window(stack)
        plugin = self.plugin = plugin_of(win, "updates")
        plugin._signals = SimpleNamespace(**{name: SimpleNamespace(emit=handler) for name, handler in (
            ("checked", plugin._on_checked), ("progress", plugin._on_progress), ("installed", plugin._on_installed))})
        plugin._driver_here = (time.monotonic(), False)  # the camera driver isn't registered from this folder
        plugin._host = HostWith(plugin._host, is_streaming=lambda: self.streaming, quit_app=lambda: None)
        plugin.set_config({**plugin.get_config(), "channel": self.channel})
        return win


def updates_scene(stack, drive, offer=200, **options):
    """The Updates & About page after drive(world, plugin) has put the plugin in one state."""
    world = World(stack, **options)
    if offer is not None:
        world.offer(offer)
    win = world.window(stack)
    drive(world, world.plugin)
    return settings_of(stack, win, "about")


def check(world, plugin):
    plugin.check(manual=True)


def checking(world, plugin):
    with mock.patch.object(type(plugin), "_spawn_check", lambda *_a: None):
        plugin.check(manual=True)


def update(world, plugin):
    """Check, then press Update and restart: it downloads and installs."""
    plugin.check(manual=True)
    plugin.update_now()


def reply_with(answer):
    """A check that the web answers with an error to raise, or a body as (bytes,)."""
    def drive(world, plugin):
        world.routes["manifest.json"] = answer
        plugin.check(manual=True)
    return drive


def downloading(done, total):
    def drive(world, plugin):
        plugin.check(manual=True)
        with mock.patch.object(type(plugin), "_spawn_install", lambda *_a, **_k: None):
            plugin.update_now()
        plugin._on_progress(done, total)
    return drive


def held_back(world, plugin):
    world.failed = 200
    plugin.check(manual=True)


def restart_fails(world, plugin):
    world.relaunch_fails = True
    update(world, plugin)


def offering(**assets):
    """An update whose files are the usual ones with these changes (a body, a size or a checksum), then updated."""
    def drive(world, plugin):
        files = world.assets(**{k: v for k, v in assets.items() if k in ("linux", "windows")})
        name = world.updates.WINDOWS_ASSET if "windows" in assets else world.updates.LINUX_ASSET
        body, size, sha = files[name]
        files[name] = (body, assets.get("size", size), assets.get("sha", sha))
        world.offer(assets=files)
        update(world, plugin)
    return drive


def download_breaks(world, plugin):
    for url in world.routes:
        if world.updates.LINUX_ASSET in url:
            world.routes[url] = OSError("reset")
    update(world, plugin)


def stream_starts_midway(world, plugin):
    """An app opens the camera while the update downloads, so a stream is running when it's installed."""
    def start():
        world.streaming = True
    world.on_read = start
    update(world, plugin)


def cancelled_midway(world, plugin):
    """Telescope is closed while the update downloads, which cancels it."""
    world.on_read = plugin._cancel.set
    update(world, plugin)


def file_in_use(world, plugin):
    """A leftover from an earlier update that can't be deleted, made as a folder in its place."""
    (world.install_folder / "TelescopeDesktop.old.exe").mkdir()
    update(world, plugin)


def cannot_replace(world, plugin):
    import update_guard
    with mock.patch.object(update_guard, "finish_swap", mock.Mock(side_effect=OSError(13, "Permission denied"))), \
            mock.patch.object(update_guard, "roll_back", lambda *_a, **_k: None):
        update(world, plugin)


def installer_fails(result=0, raises=None):
    def drive(world, plugin):
        world.installer.result, world.installer.raises = result, raises
        update(world, plugin)
    return drive


def unexpected_in(name):
    """Something unexpected goes wrong in one step of checking or installing."""
    def drive(world, plugin):
        with mock.patch.object(world.updates, name, mock.Mock(side_effect=RuntimeError("boom"))):
            update(world, plugin)
    return drive


NOT_FOUND = urllib.error.HTTPError("https://example.com", 404, "", {}, None)
SERVER_ERROR = urllib.error.HTTPError("https://example.com", 503, "", {}, None)
WINDOWS = {"windows": True}
UNPACKAGED = {"windows": True, "packaged": False}
UNZIPPED = {"windows": True, "unzipped": True}
SOURCE = {"app_channel": "dev"}
UPDATE_STATES = (
    ("checking", "checking for new versions", checking, {}),
    ("current", "you have the latest version", check, {"build": 200}),
    ("found", "new version found", check, {}),
    ("found-nightly", "new nightly found", check, {"channel": "nightly"}),
    ("found-streaming", "new version found while streaming", check, {"streaming": True}),
    ("found-moves", "new version found, installed for your user", check, UNZIPPED),
    ("found-source", "new version found in a source checkout", check, SOURCE),
    ("found-unpackaged", "new version found, only the packaged app updates", check, UNPACKAGED),
    ("found-readonly", "new version found, folder not writable", check, {"writable": False}),
    ("downloading", "downloading", downloading(40_000, 100_000), {}),
    ("downloading-unknown", "downloading, size unknown", downloading(0, 0), {}),
    ("installing", "installing", downloading(-1, -1), {}),
    ("restart-pending", "installed, restarts after streaming", stream_starts_midway, {}),
    ("restart-failed", "installed, restart failed", restart_fails, {}),
    ("held-back", "new version held back", held_back, {}),
    ("source-checkout", "source checkout, latest stable", check, {**SOURCE, "build": 0}),
    ("source-checkout-nightly", "source checkout, latest nightly", check, {**SOURCE, "build": 0, "channel": "nightly"}),
    ("no-release", "no stable release yet", reply_with(NOT_FOUND), {}),
    ("no-release-nightly", "no nightly release yet", reply_with(NOT_FOUND), {"channel": "nightly"}),
    ("check-http", "check failed, server error", reply_with(SERVER_ERROR), {}),
    ("check-certificate", "check failed, certificate", reply_with(urllib.error.URLError(ssl.SSLCertVerificationError())), {}),
    ("check-offline", "check failed, offline", reply_with(urllib.error.URLError(OSError("offline"))), {}),
    ("check-unreadable", "check failed, reply unreadable", reply_with((b"not json",)), {}),
    ("check-unexpected", "check failed, unexpected", unexpected_in("fetch_manifest"), {}),
    ("download-cancelled", "download cancelled", cancelled_midway, {}),
    ("download-failed", "download failed", download_breaks, {}),
    ("download-too-big", "download bigger than expected", offering(size=3), {}),
    ("download-short", "download cut short", offering(size=10_000_000), {}),
    ("download-checksum", "download checksum wrong", offering(sha="0" * 64), {}),
    ("install-paths", "update has unexpected paths", offering(windows=zip_of({"../outside.txt": b""})), WINDOWS),
    ("install-files", "update has unexpected files", offering(linux=tar_of({"main.py": b""}, symlink="link")), {}),
    ("install-unpack", "update can't be unpacked", offering(linux=b"not an archive"), {}),
    ("install-empty", "update has no Telescope in it", offering(windows=zip_of({"readme.txt": b""})), WINDOWS),
    ("install-in-use", "update file in use", file_in_use, WINDOWS),
    ("install-replace", "update can't replace files", cannot_replace, {}),
    ("install-unexpected", "update failed unexpectedly", unexpected_in("download_update"), {}),
    ("installer-slow", "installer took too long", installer_fails(raises=subprocess.TimeoutExpired("setup", 600)), UNZIPPED),
    ("installer-missing", "installer won't start", installer_fails(raises=OSError(2, "No such file")), UNZIPPED),
    ("installer-stopped", "installer stopped", installer_fails(result=5), UNZIPPED),
    ("installer-lost", "installed but not found", installer_fails(), UNZIPPED),
)


def _updates_scene(id, title, drive, options):
    scene(f"updates-{id}", "states", f"Updates · {title}")(lambda stack: updates_scene(stack, drive, **options))


for _id, _title, _drive, _options in UPDATE_STATES:
    _updates_scene(_id, _title, _drive, _options)


@scene("main-update-found", "main", "Main window · new version found")
def main_update_found(stack):
    import telescope.app as app_module
    world = World(stack)
    world.offer()
    win = world.window(stack)
    world.plugin.check(manual=True)
    grow(win, app_module._WIDTH_THREE_COL, 900)
    return win


# ── Monitoring ───────────────────────────────────────────────────────────────

class Client:
    """What stands in for a phone's control client: only its identity matters."""


def monitoring_window(stack, streaming=True, **config):
    """The real window with Monitoring's settings, and a host that says a stream runs (and stopping it does nothing)."""
    win = main_window(stack)
    plugin = plugin_of(win, "monitoring")
    plugin.set_config({**plugin.get_config(), **config})
    phone = Client()
    plugin._host = HostWith(plugin._host, is_streaming=lambda: streaming, stop_stream=lambda *_a: None,
                            start_again=lambda _sid: (lambda: None),
                            background_streams=lambda: [("phone-2", "Pixel 8", phone)],
                            device_config=lambda _sid, _name: config)
    return win, plugin, phone


def monitoring_notice(stack, *readings, other=False, **config):
    """Notifications the Monitoring plugin sends for phone readings in a row, drawn as the desktop shows them."""
    sent = capture_notifications(stack)
    _win, plugin, phone = monitoring_window(stack, **config)
    for reading in readings:
        if other:
            plugin._on_other_polled("phone-2", "Pixel 8", phone, reading)
        else:
            plugin._on_state(reading)
    return bubble(stack, sent)


def monitoring_banner(stack, reading, other=False, **config):
    """The main window after Monitoring stopped a stream: the banner it leaves above the cards."""
    import telescope.app as app_module
    win, plugin, phone = monitoring_window(stack, **config)
    if other:
        plugin._on_other_polled("phone-2", "Pixel 8", phone, reading)
    else:
        plugin._on_state(reading)
    grow(win, app_module._WIDTH_THREE_COL, 900)
    return win


LOW = {"battery": 14, "charging": False, "battery_temp_c": 30.0}
DROPPING = {"battery": 14, "charging": True, "battery_temp_c": 30.0}
HOT = {"battery": 64, "charging": False, "battery_temp_c": 48.2}
STOP_LOW = {"battery_stop": True}
STOP_HOT = {"temp_stop": True}
NOTICES = (
    ("battery-low", "battery low", (LOW,), {}),
    ("battery-dropping", "battery dropping while charging", ({**DROPPING, "battery": 16}, DROPPING), {}),
    ("battery-stopped", "battery low, stream stopped", (LOW,), STOP_LOW),
    ("hot", "phone running hot", (HOT,), {}),
    ("hot-stopped", "phone running hot, stream stopped", (HOT,), STOP_HOT),
    ("hot-named", "another phone running hot", (HOT,), {"other": True}),
)
BANNERS = (
    ("stopped-battery", "stopped for a low battery", LOW, STOP_LOW),
    ("stopped-battery-named", "another phone stopped for a low battery", LOW, {"other": True, **STOP_LOW}),
    ("stopped-hot", "stopped for heat", HOT, STOP_HOT),
    ("stopped-hot-named", "another phone stopped for heat", HOT, {"other": True, **STOP_HOT}),
)


def _notice_scene(id, title, readings, options):
    scene(f"notify-{id}", "notifications", f"Notification · {title}")(
        lambda stack: monitoring_notice(stack, *readings, **options))


def _banner_scene(id, title, reading, options):
    scene(f"monitoring-{id}", "states", f"Monitoring · {title}")(lambda stack: monitoring_banner(stack, reading, **options))


for _id, _title, _readings, _options in NOTICES:
    _notice_scene(_id, _title, _readings, _options)
for _id, _title, _reading, _options in BANNERS:
    _banner_scene(_id, _title, _reading, _options)


# ── Startup ──────────────────────────────────────────────────────────────────

@scene("settings-startup-dev", "settings", "Settings · Startup in a dev profile")
def startup_in_dev_profile(stack):
    import telescope.plugins.startup as startup
    stack.enter_context(mock.patch.object(startup, "dev_profile", SimpleNamespace(active=lambda: True)))
    return settings_of(stack, main_window(stack), "general")


@scene("settings-automatic-stop", "settings", "Settings · Automatic streaming, stopping by itself")
def automatic_stop(stack):
    win = main_window(stack)
    plugin = plugin_of(win, "startup")
    plugin.set_config({"start_on": "watched", "stop_idle": "own", "stop_delay_s": 90, "notify_stop": True})
    return settings_of(stack, win, "streaming")


def idle_stop_notice(stack, seconds):
    """The notification after Automatic streaming stopped a stream that no app was using."""
    from telescope.plugins.startup import delay_text
    sent = capture_notifications(stack)
    win = main_window(stack)
    plugin = plugin_of(win, "startup")
    plugin.set_config({"start_on": "watched", "stop_idle": "any", "stop_delay_s": seconds, "notify_stop": True})
    plugin._host = HostWith(plugin._host, is_streaming=lambda: True, is_starting=lambda: False, is_camera_on=lambda: True,
                            stream_count=lambda: 1, can_turn_camera_off=lambda: (False, ""), stop_stream=lambda *_a: None)
    stack.callback(plugin._idle_stop.stop)
    plugin._on_stream_started("")
    plugin._on_camera_watched(False)
    plugin._stop_idle()
    # The wait is a translated text of its own, put into the sentence, which keeps only the sentence's tag
    wait = delay_text(seconds)
    return bubble(stack, [(title, Joined(str(body), parts_of(body) + parts_of(wait)), urgent) for title, body, urgent in sent])


@scene("notify-idle-stop", "notifications", "Notification · stopped after no app used the camera")
def notify_idle_stop(stack):
    return idle_stop_notice(stack, 90)


@scene("notify-idle-stop-minutes", "notifications", "Notification · stopped after no app used the camera, in minutes")
def notify_idle_stop_minutes(stack):
    return idle_stop_notice(stack, 120)


# ── Splash and joined lists ──────────────────────────────────────────────────

@scene("splash-windows", "states", "Splash · Windows first start")
def splash_screen(stack):
    """The picture the launcher shows while Windows checks the new files, with the text splash.py puts on it."""
    import sys
    from PyQt6.QtCore import QRect, Qt
    from PyQt6.QtGui import QPixmap
    from PyQt6.QtWidgets import QLabel
    from telescope import i18n, splash
    texts, written = [], []

    class Launcher:
        """pyi_splash, the module the packaged launcher gives the app."""

        @staticmethod
        def is_alive():
            return True

        @staticmethod
        def update_text(text):
            written.append(text)

    def translate(*args, **kwargs):
        texts.append(i18n.tr(*args, **kwargs))
        return texts[-1]

    stack.enter_context(mock.patch.dict(sys.modules, {"pyi_splash": Launcher}))
    stack.enter_context(mock.patch.object(splash, "tr", translate))
    stack.enter_context(mock.patch.object(splash, "_seconds_since_launch", lambda: 60.0))
    stack.enter_context(mock.patch.object(splash, "_windows_languages", lambda: []))
    stack.enter_context(mock.patch.object(i18n, "load", lambda *_a, **_k: None))  # the preview has its language already
    splash.show_text(lambda: "")
    text = Joined(written[-1], [part for shown_text in texts for part in parts_of(shown_text)])
    label = QLabel()
    pixmap = QPixmap(str(Path(__file__).resolve().parent.parent / "resources" / "splash.png"))
    label.setPixmap(pixmap)
    label.setFixedSize(pixmap.size())
    caption = QLabel(text, label)
    caption.setStyleSheet("color: #a1abb8; font-size: 10pt; background: transparent;")
    caption.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignBottom)
    caption.setGeometry(QRect(28, 20, pixmap.width() - 56, pixmap.height() - 20 - 24))
    remember(caption, "text", text)
    return shown(stack, label)


@scene("main-plugins-off", "main", "Main window · plugins turned off")
def plugins_turned_off(stack):
    """The banner listing the plugins switched off: the list joins their names with the commas and the 'and'."""
    import telescope.app as app_module
    from telescope import i18n, plugin_loader
    from telescope.plugins import builtin_plugins
    seen = []
    plain = i18n.tr

    def translate(*args, **kwargs):
        seen.append(plain(*args, **kwargs))
        return seen[-1]

    win = main_window(stack)
    off = ("framing", "preview", "background")
    win._builtin_plugins = builtin_plugins()
    stack.enter_context(mock.patch.object(plugin_loader, "unnoticed_off", lambda: set(off)))
    stack.enter_context(mock.patch.object(i18n, "tr", translate))
    win._remind_plugins_off()
    grow(win, app_module._WIDTH_THREE_COL, 900)
    title = win._banners._banners["plugins_off"].title_lbl
    # and_list's own texts are filled into the banner's, which keeps only the banner's tag
    joined = [part for text in seen for part in parts_of(text) if str(part) in title.text()]
    remember(title, "text", Joined(title.text(), joined))
    return win
