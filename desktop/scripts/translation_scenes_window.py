"""Scenes for the main window, plugins panel and plugin loader: see translation_scenes.py for how scenes are registered."""

import functools
import zipfile
import threading
import types
from unittest import mock

from scripts.translation_scenes import *  # noqa: F401,F403
from scripts.translation_scenes import _Held, _MicBackend, quiet_sign, remember


def also_shows(target, slot, *texts):
    """A text from tr() that went into another tr() as a field loses its tag: note it on what shows the whole sentence."""
    from scripts.translation_scenes import parts_of
    current = getattr(target, "_preview_texts", {}).get(slot)
    shown_text = str(current) if current is not None else ""
    remember(target, slot, Joined(shown_text, parts_of(current) + [p for t in texts for p in parts_of(t)]))


@functools.cache
def fake_worker_class():
    from PyQt6.QtCore import QObject, pyqtSignal

    class FakeWorker(QObject):
        """A stream worker that reads nothing: the signals and calls the window makes of one."""

        status = pyqtSignal(str, str)
        reconnected = pyqtSignal()
        vcam_opened = pyqtSignal(int, int)
        last_arrival_fps = 0.0

        def request_stop(self):
            pass

        def wait(self, _ms):
            return True

        def settle(self):
            pass

        def set_pipeline(self, _pipeline):
            pass

        def set_parked(self, _parked):
            pass

        def latest_frame(self):
            return None

    return FakeWorker


class FakeClient:
    auth = None

    def send(self, **_kwargs):
        pass

    def close(self):
        pass


def streaming(win, source, slot=0, connected=True):
    """source streams to virtual camera slot, with a worker that does nothing, and the panels show it if nothing else."""
    from telescope.session import StreamSession
    sessions = win._sessions
    session = StreamSession(id=len(sessions.streams) + 1, url="http://192.168.1.20:8080/v1/video", client=FakeClient(),
                            worker=fake_worker_class()(), source=source)
    first = not sessions.streams
    sessions.add(source, session)
    if sessions.focus is None:
        sessions.focus = source
    if sessions.main is None:
        sessions.main = source
    if first:
        win._bus.stream_started.emit(session.url)
    source.slot = slot
    source.connected = connected
    source.status = ("Live", "ok") if connected else ("Connecting", "dim")
    return session


class NoThread:
    """Stands in for a thread: what it would have done in the background doesn't happen."""

    def __init__(self, *_args, **_kwargs):
        pass

    def start(self):
        pass


def quiet_phones(stack):
    """Phones that never reach out: nothing is fetched, woken or stopped over the network."""
    from telescope import sources
    for name in ("refresh_state", "started", "_stop_phone_async", "watch_alive", "camera_turned_on", "lost", "wake"):
        stack.enter_context(mock.patch.object(sources.PhoneSource, name, lambda self, *_a, **_k: None))


def live_window(stack, tray=False, phone=True, phones=3):
    """The window with phones paired (the first one picked) whose streams can be faked."""
    quiet_phones(stack)
    win = main_window(stack, tray=tray, phone=phone)
    if phone:
        connection = win._plugin("connection")
        config = connection.get_config()
        config["phones"] = [{"id": f"phone-{n}", "name": name, "token": "token", "ips": [], "active_ip": None}
                            for n, name in enumerate(("Pixel 8", "Galaxy S23", "Moto G"), 1)][:phones]
        connection.set_config(config)
        win._bus.phones_changed.emit(phones)
    return win


def phone_source(win, pid="phone-1"):
    return win._phone_source(pid)


def refresh_streaming(win):
    win._set_start_button(streaming=True)
    win._flow.show_focus()
    win._show_tray_state()


def window_wide(win):
    import telescope.app as app_module
    grow(win, app_module._WIDTH_THREE_COL, 900)
    return win


def streaming_window(stack, tray=False):
    """One phone streaming, live."""
    win = live_window(stack, tray=tray)
    streaming(win, phone_source(win))
    refresh_streaming(win)
    return win


@scene("main-streaming-narrow", "main", "Main window · streaming, narrow")
def main_streaming_narrow(stack):
    win = streaming_window(stack)
    grow(win, 560, 900)
    return win


@scene("main-streaming-tiles", "states", "Several cameras · live, connecting and reconnecting")
def main_tiles(stack):
    win = live_window(stack)
    streaming(win, phone_source(win))
    streaming(win, phone_source(win, "phone-2"), slot=1, connected=False)
    third = phone_source(win, "phone-3")
    streaming(win, third, slot=2)
    third.recovering = True
    refresh_streaming(win)
    return window_wide(win)


@scene("main-starting-tiles", "states", "Several cameras · one starting, one waiting")
def main_waiting_tiles(stack):
    win = live_window(stack)
    streaming(win, phone_source(win))
    sessions = win._sessions
    sessions.waking = True
    sessions.wake_source = phone_source(win, "phone-2")
    sessions.pending = phone_source(win, "phone-3")
    refresh_streaming(win)
    return window_wide(win)


# ── Status line and banners of the window ────────────────────────────────────

def without_threads(stack):
    """The window's background work (setting up cameras, reloading the driver) is left out of the picture."""
    import telescope.app as app_module
    stack.enter_context(mock.patch.object(app_module, "threading", types.SimpleNamespace(Thread=NoThread, Event=threading.Event)))


def on_platform(stack, linux):
    import telescope.app as app_module
    stack.enter_context(mock.patch.object(app_module, "IS_LINUX", linux))


def names_text():
    """The "{first} to {last}" the virtual cameras' question puts in its sentence."""
    from telescope import vcam
    return i18n.tr("{first} to {last}", first=vcam.slot_label(1), last=vcam.slot_label(vcam.MAX_SLOTS - 1))


def slots_question(stack, linux):
    held = hold_dialogs(stack)
    win = live_window(stack)
    on_platform(stack, linux)
    win._ask_slots()
    box = held[-1]
    remember(box, "window title", i18n.tr("Set up more virtual cameras"))
    also_shows(box, "text", names_text())
    return shown(stack, box)


@scene("dialog-slots-linux", "dialogs", "Set up more virtual cameras · Linux")
def slots_linux(stack):
    return slots_question(stack, True)


@scene("dialog-slots-windows", "dialogs", "Set up more virtual cameras · Windows")
def slots_windows(stack):
    return slots_question(stack, False)


@scene("state-slots-setting-up", "states", "Setting up more virtual cameras")
def slots_setting_up(stack):
    from PyQt6.QtWidgets import QMessageBox
    hold_dialogs(stack)
    stack.enter_context(mock.patch.object(QMessageBox, "exec", lambda self: QMessageBox.StandardButton.Ok))
    without_threads(stack)
    win = live_window(stack)
    win._set_up_slots(lambda: None)
    return window_wide(win)


@scene("state-slots-failed", "states", "Couldn't add more virtual cameras")
def slots_failed(stack):
    win = live_window(stack)
    win._on_slots_ready(False, "The extra cameras couldn't be set up.", "sudo modprobe -r v4l2loopback")
    return window_wide(win)


def canvas_win(stack, linux):
    without_threads(stack)
    on_platform(stack, linux)
    return live_window(stack)


@scene("state-canvas-reloading", "states", "Resizing the virtual camera · reloading the driver")
def canvas_reloading(stack):
    win = canvas_win(stack, True)
    win.restart_vcam_canvas(1280, 720)
    return window_wide(win)


@scene("state-canvas-restarting", "states", "Resizing the virtual camera · restarting the stream")
def canvas_restarting(stack):
    win = canvas_win(stack, False)
    win.restart_vcam_canvas(1280, 720)
    return window_wide(win)


@scene("state-canvas-reloaded", "states", "Resizing the virtual camera · driver reloaded")
def canvas_reloaded(stack):
    win = canvas_win(stack, True)
    win._on_canvas_reload_done(True, "Loaded v4l2loopback", False)
    return window_wide(win)


@scene("state-canvas-updated", "states", "Resizing the virtual camera · canvas updated")
def canvas_updated(stack):
    win = canvas_win(stack, False)
    win._on_canvas_reload_done(True, "", False)
    return window_wide(win)


@scene("state-canvas-failed", "states", "Resizing the virtual camera · reload failed")
def canvas_failed(stack):
    win = canvas_win(stack, True)
    win._on_canvas_reload_done(False, "modprobe: Operation not permitted", False)
    return window_wide(win)


@scene("state-canvas-by-hand", "states", "Resizing the virtual camera · a command to run by hand")
def canvas_by_hand(stack):
    win = canvas_win(stack, True)
    win._on_canvas_reload_done(False, "The driver is in use.", False, "sudo modprobe -r v4l2loopback && sudo modprobe v4l2loopback")
    return window_wide(win)


@scene("state-reconnecting", "states", "The phone stopped answering with its camera off")
def lost_contact(stack):
    win = streaming_window(stack)
    win._source_host.lost_contact(phone_source(win))
    win._stop_reconnecting_animation()
    return window_wide(win)


@scene("state-start-problem", "states", "Couldn't start streaming")
def start_problem(stack):
    from telescope import sources
    def refuse(self, interactive):
        raise RuntimeError("no way to the phone")
    stack.enter_context(mock.patch.object(sources.PhoneSource, "prepare", refuse))
    win = live_window(stack)
    win._flow.start()
    return window_wide(win)


@scene("state-stopped-unexpectedly", "states", "The stream stopped by itself")
def stopped_unexpectedly(stack):
    win = streaming_window(stack)
    win._on_worker_status("idle", "Stopped", phone_source(win))
    return window_wide(win)


@scene("state-stopped-unexpectedly-other", "states", "A stream next to the shown one stopped by itself")
def stopped_unexpectedly_other(stack):
    win = live_window(stack)
    streaming(win, phone_source(win))
    other = phone_source(win, "phone-2")
    streaming(win, other, slot=1)
    refresh_streaming(win)
    win._on_worker_status("idle", "Stopped", other)
    return window_wide(win)


@scene("state-phone-data", "states", "The phone sent data this version can't read")
def phone_data(stack):
    win = streaming_window(stack)
    source = phone_source(win)
    win._apply_state(source.session.id, {"battery": "full"}, source)
    return window_wide(win)


def slow_stream(stack, dynamic=0, heat=0):
    win = streaming_window(stack)
    source = phone_source(win)
    source.dynamic_fps, source.heat_fps = dynamic, heat
    win._show_slow(True, camera_limited=True)
    return window_wide(win)


@scene("state-slow-camera", "states", "Frames are slow · the camera makes fewer")
def slow_camera(stack):
    return slow_stream(stack)


@scene("state-slow-dynamic", "states", "Frames are slow · Dynamic lowered the rate")
def slow_dynamic(stack):
    return slow_stream(stack, dynamic=24)


@scene("state-slow-heat", "states", "Frames are slow · the phone's heat lowered the rate")
def slow_heat(stack):
    return slow_stream(stack, heat=20)


# ── Camera off: the mic alone ────────────────────────────────────────────────

def mic_on(win):
    mic = win._plugin("microphone")
    mic.set_config({**mic.get_config(), "enabled": True})


@scene("main-mic-only", "main", "Main window · Start mic only")
def mic_only(stack):
    win = live_window(stack)
    mic_on(win)
    win.set_camera_on(False)
    return window_wide(win)


@scene("main-camera-off", "states", "Streaming the mic, camera off")
def camera_off(stack):
    win = streaming_window(stack)
    mic_on(win)
    win.set_camera_on(False)
    return window_wide(win)


@scene("state-camera-turning-on", "states", "Turning the phone's camera back on")
def camera_turning_on(stack):
    win = streaming_window(stack)
    mic_on(win)
    win.set_camera_on(False)
    win._flow.start_worker = lambda url, auth, source: fake_worker_class()()
    win.set_camera_on(True)
    return window_wide(win)


def cannot_turn_off(stack, setup):
    win = streaming_window(stack)
    setup(win)
    win._plugin("preview")._show_camera_btn()
    return window_wide(win)


def not_a_phone(win):
    phone_source(win).mic_alone = False


def second_stream(win):
    streaming(win, phone_source(win, "phone-2"), slot=1)
    refresh_streaming(win)


def phone_too_old(win):
    mic_on(win)
    phone_source(win).camera_toggle = False


for _id, _title, _setup in (
        ("camera-off-not-a-phone", "Camera off unavailable · not a phone", not_a_phone),
        ("camera-off-several", "Camera off unavailable · several cameras", second_stream),
        ("camera-off-no-mic", "Camera off unavailable · phone mic off", lambda win: None),
        ("camera-off-old-phone", "Camera off unavailable · phone too old", phone_too_old)):
    scene(_id, "states", _title)(lambda stack, setup=_setup: cannot_turn_off(stack, setup))


# ── Notifications of the window ──────────────────────────────────────────────

@scene("notify-save-failed", "notifications", "Notification · settings couldn't be saved")
def notify_save_failed(stack):
    import telescope.app as app_module
    sent = capture_notifications(stack)
    win = live_window(stack)
    stack.enter_context(mock.patch.object(app_module, "save_config", lambda _cfg: False))
    win.save_now()
    return bubble(stack, sent)


def closing_to_tray(stack, streaming_now):
    from PyQt6.QtGui import QCloseEvent
    sent = capture_notifications(stack)
    win = live_window(stack, tray=True)
    if streaming_now:
        streaming(win, phone_source(win))
    else:
        win._keep_in_tray = True
    win.closeEvent(QCloseEvent())
    return bubble(stack, sent)


@scene("notify-still-running-streaming", "notifications", "Notification · closed while streaming")
def still_running_streaming(stack):
    return closing_to_tray(stack, True)


@scene("notify-still-running-waiting", "notifications", "Notification · closed while waiting to stream")
def still_running_waiting(stack):
    return closing_to_tray(stack, False)


@scene("banner-settings-reset", "states", "Your settings couldn't be read")
def settings_reset(stack):
    from telescope.config import config_path
    win = live_window(stack)
    config_path().parent.mkdir(parents=True, exist_ok=True)
    config_path().write_text("{ not json", encoding="utf-8")
    win.apply_saved_config()
    return window_wide(win)


# ── Window built the way the app starts it: plugins loaded and turned off as saved ──

def tweak_plugins(plugins):
    """The plugins of a window that must not reach the system, as main_window sets them."""
    for plugin in plugins:
        if plugin.name == "wait_screen":
            plugin._screen, plugin._watch_cls, plugin._extra_screen = _Held(), _Held, _Held
        elif plugin.name == "microphone":
            plugin._backend = _MicBackend()
        elif plugin.name == "shortcuts":
            plugin._backend_factory = lambda: None
        elif plugin.name == "on_air":
            quiet_sign(plugin)


def loaded_window(stack, before=None, tray=False, dev=False):
    """The real window started through load_plugins, after before() set up the profile (which plugins are off, added ones)."""
    from PyQt6.QtWidgets import QApplication
    import telescope.app as app_module
    from telescope.app import TelescopeWindow
    from telescope.plugins import builtin_plugins
    quiet_phones(stack)
    use_scratch_profile(stack)
    quiet_system(stack, tray)
    if before is not None:
        before()
    if dev:
        class DevProfile:
            """The window's own view of dev_profile, saying this is a dev copy; the settings file stays where it is."""
            active = staticmethod(lambda: True)

            def __getattr__(self, name):
                return getattr(real, name)
        real = app_module.dev_profile
        stack.enter_context(mock.patch.object(app_module, "dev_profile", DevProfile()))
    win = TelescopeWindow()
    builtins = builtin_plugins()
    tweak_plugins(builtins)
    win.load_plugins(builtins)
    win.apply_saved_config()
    connection = win._plugin("connection")
    connection.set_config({**connection.get_config(), "selected_phone": "phone-1", "phones": [
        {"id": "phone-1", "name": "Pixel 8", "token": "token", "ips": [], "active_ip": None}]})
    QApplication.processEvents()
    stack.callback(close_window, win)
    return win


@scene("main-dev", "main", "Main window · a development copy")
def main_dev(stack):
    return window_wide(loaded_window(stack, dev=True))


@functools.cache
def fake_download_class():
    from PyQt6.QtCore import QObject, pyqtSignal

    class FakeDownload(QObject):
        """The vision pack's download in whatever state a scene needs."""

        progress = pyqtSignal(int, int)
        finished = pyqtSignal(str)

        def __init__(self, ready=False, available=True, running=False, waiting="", error="", failed_for="", remove_error=""):
            super().__init__()
            self._ready, self._available, self._running = ready, available, running
            self.waiting, self.error, self.failed_for = waiting, error, failed_for
            self._remove_error = remove_error

        def ready(self):
            return self._ready

        def available(self):
            return self._available

        def running(self):
            return self._running

        def size_mb(self):
            return 85

        def start(self, switch_on=""):
            pass

        def cancel(self):
            pass

        def remove(self):
            if self._remove_error:
                raise OSError(self._remove_error)

    return FakeDownload


GOOD_PLUGIN = '''from telescope.plugin import TelescopePlugin


class {cls}(TelescopePlugin):
    name = "{name}"
'''


def plugin_loader():
    from telescope import plugin_loader as module
    return module


def write_plugin(name, manifest=None, drop=(), init=True, extra=None, folder=None):
    """A plugin folder in the plugins folder: its plugin.json, an __init__.py with a working plugin, and any extra files."""
    import json
    path = plugin_loader().make_plugins_dir() / (folder or name)
    path.mkdir()
    path.chmod(0o755)
    info = {"name": name, "title": name.replace("_", " ").title(), "version": "1.0", "author": "Sam", "api": 1,
            "class": "Demo", "description": "Adds a small card with a counter.", **(manifest or {})}
    for dropped in drop:
        info.pop(dropped, None)
    (path / "plugin.json").write_text(json.dumps(info), encoding="utf-8")
    if init:
        (path / "__init__.py").write_text(GOOD_PLUGIN.format(cls=info.get("class") or "Demo", name=name), encoding="utf-8")
    for rel, content in (extra or {}).items():
        (path / rel).write_bytes(content)
    for item in path.rglob("*"):
        if item.is_file() and not item.is_symlink():
            item.chmod(0o644)
    return path


def installed(name):
    return next(i for i in plugin_loader().find_installed() if i.name == name)


def turn_on(name, on=True):
    """The added plugin called name, switched on as its files are now (or off)."""
    plugin_loader().set_enabled(installed(name), on)


def plugins_context(stack, before=None, after=None, download=None, streaming_now=False, tab="plugins"):
    """The window, its Settings window on the Plugins tab and that tab's panel; after() changes the profile once the
    app is up, which is what rows that wait for a restart show."""
    from PyQt6.QtWidgets import QApplication
    from telescope.widgets import vision_download
    from telescope.widgets.plugins_panel import PluginsPanel
    from telescope.widgets.settings_window import _SIZE
    stack.enter_context(mock.patch.object(vision_download, "shared", lambda: download or fake_download_class()()))
    win = loaded_window(stack, before)
    if streaming_now:
        streaming(win, phone_source(win))
    if after is not None:
        after(win)
    win.resize(1380, 900)
    win.open_settings(tab)
    settings = win._settings_win
    QApplication.processEvents()
    grow(settings, _SIZE[0], _SIZE[1])
    return win, settings, settings.findChild(PluginsPanel)


def plugins_scene(stack, **kwargs):
    return plugins_context(stack, **kwargs)[1]


def nest(top, *texts):
    """Add the tags of texts that went into another tr() as a field, wherever a shown sentence holds them."""
    from PyQt6.QtWidgets import QWidget
    from scripts.translation_scenes import parts_of
    for widget in [top, *top.findChildren(QWidget)]:
        for slot, current in list(getattr(widget, "_preview_texts", {}).items()):
            fresh = [t for t in texts if str(t) in str(current) and str(t) != str(current)]
            if fresh and parts_of(current):
                also_shows(widget, slot, *fresh)


# ── The Plugins page of Settings ─────────────────────────────────────────────

@scene("plugins-restart-rows", "settings", "Plugins · changes that wait for a restart")
def plugins_restart_rows(stack):
    def before():
        plugin_loader().set_builtin_off("transforms", True)
        write_plugin("counter_card")
        write_plugin("clock_card", {"title": "Clock card"})
        turn_on("clock_card")

    def after(_win):
        loader = plugin_loader()
        loader.set_builtin_off("monitoring", True)
        loader.set_builtin_off("transforms", False)
        loader.set_optional_on("framing", True)
        turn_on("counter_card")
        turn_on("clock_card", False)

    return plugins_scene(stack, before=before, after=after, download=fake_download_class()(ready=True))


@scene("plugins-restart-bar", "settings", "Plugins · restart bar after a plugin was removed")
def plugins_restart_bar(stack):
    def before():
        write_plugin("counter_card")
        turn_on("counter_card")

    def after(_win):
        plugin_loader().remove(installed("counter_card"))

    return plugins_scene(stack, before=before, after=after)


def optional_page(stack, download, framing_on=False, progress=None):
    def before():
        if framing_on:
            plugin_loader().set_optional_on("framing", True)

    win, settings, panel = plugins_context(stack, before=before, download=download)
    if progress is not None:
        download.progress.emit(*progress)
    return settings


@scene("plugins-optional-offline", "settings", "Plugins · optional ones with no download for this computer")
def plugins_optional_offline(stack):
    return optional_page(stack, fake_download_class()(available=False))


@scene("plugins-optional-needs-download", "settings", "Plugins · optional ones that need a download first")
def plugins_optional_needs_download(stack):
    return optional_page(stack, fake_download_class()())


@scene("plugins-optional-downloading", "settings", "Plugins · downloading for an optional one")
def plugins_optional_downloading(stack):
    return optional_page(stack, fake_download_class()(running=True, waiting="framing"), progress=(34_000_000, 85_000_000))


@scene("plugins-optional-failed", "settings", "Plugins · the download didn't work")
def plugins_optional_failed(stack):
    return optional_page(stack, fake_download_class()(failed_for="framing", error="Couldn't reach github.com."))


@scene("plugins-optional-missing", "settings", "Plugins · an optional one is on but its download is gone")
def plugins_optional_missing(stack):
    return optional_page(stack, fake_download_class()(), framing_on=True)


@scene("plugins-optional-ready", "settings", "Plugins · the download is there")
def plugins_optional_ready(stack):
    return optional_page(stack, fake_download_class()(ready=True))


# ── Added plugins that can't be loaded: what the loader says about each folder ──

def unreadable_file(path):
    (path / "notes.txt").write_text("x", encoding="utf-8")
    (path / "notes.txt").chmod(0)


def problem_folders():
    """(folder name, how to make the folder) for each way a plugin folder is turned down while it's looked at."""
    import os

    def link_to_folder(_name):
        target = plugin_loader().make_plugins_dir() / ".elsewhere"
        target.mkdir()
        os.symlink(target, plugin_loader().make_plugins_dir() / "folder_link")

    def make(name, manifest=None, **kwargs):
        return lambda _n: write_plugin(name, manifest, **kwargs)

    def shared(name):
        path = write_plugin(name)
        path.chmod(0o777)

    def linked(name):
        path = write_plugin(name)
        os.symlink(path / "plugin.json", path / "shortcut.json")

    def too_big(name):
        path = write_plugin(name)
        for n in range(501):
            (path / f"part_{n}.txt").write_text("", encoding="utf-8")
            (path / f"part_{n}.txt").chmod(0o644)

    def unreadable(name):
        unreadable_file(write_plugin(name))

    def bad_json(name):
        path = write_plugin(name)
        (path / "plugin.json").write_text("{ nope", encoding="utf-8")

    def not_an_object(name):
        path = write_plugin(name)
        (path / "plugin.json").write_text("[1, 2]", encoding="utf-8")

    def no_manifest(name):
        path = write_plugin(name)
        (path / "plugin.json").unlink()

    return [
        ("folder_link", link_to_folder), ("Bad-Name", make("bad_name", folder="Bad-Name")),
        ("no_manifest", no_manifest), ("bad_json", bad_json), ("not_an_object", not_an_object),
        ("other_name", make("other_name", {"name": "something_else"})),
        ("monitoring", make("monitoring")), ("too_new", make("too_new", {"api": 99})),
        ("too_old", make("too_old", {"api": 0})), ("no_class", make("no_class", {"class": ""})),
        ("no_init", make("no_init", init=False)), ("shared", shared), ("linked", linked),
        ("compiled", make("compiled", extra={"helper.pyc": b"x"})), ("too_big", too_big), ("unreadable", unreadable),
    ]


def problems_page(stack, names):
    chosen = {name: build for name, build in problem_folders() if name in names}
    if "unreadable" in chosen:
        # chmod doesn't stop root, which builds the hosted image, so the read fails here as it would for anyone else
        from telescope import plugin_loader
        real = plugin_loader.digest

        def digest(path, *args, **kwargs):
            if path.name == "unreadable":
                raise PermissionError("notes.txt")
            return real(path, *args, **kwargs)
        stack.enter_context(mock.patch.object(plugin_loader, "digest", digest))

    def before():
        for name, build in chosen.items():
            build(name)
    return plugins_scene(stack, before=before)


_FOLDERS = [name for name, _build in problem_folders()]
for _part, _names in enumerate((_FOLDERS[:8], _FOLDERS[8:]), 1):
    scene(f"plugins-problems-{_part}", "settings", f"Plugins · added folders that can't load, part {_part}")(
        lambda stack, names=tuple(_names): problems_page(stack, names))


# ── Added plugins that start badly, and the banner on the window ──

BROKEN_CLASS = '''from telescope.plugin import TelescopePlugin


class Demo(TelescopePlugin):
    name = "{other}"
'''


def failing_plugins():
    """Added plugins that are on and fail while loading, with the texts the loader gave for them as tr() results."""
    write_plugin("not_a_class", {"class": "helper"})
    (plugin_loader().plugins_dir() / "not_a_class" / "__init__.py").write_text("helper = 1\n", encoding="utf-8")
    write_plugin("wrong_name")
    (plugin_loader().plugins_dir() / "wrong_name" / "__init__.py").write_text(BROKEN_CLASS.format(other="other_one"), encoding="utf-8")
    for name in ("not_a_class", "wrong_name"):
        turn_on(name)


def failing_texts():
    return [i18n.tr("{name} isn't a TelescopePlugin class.", name="helper"),
            i18n.tr("Its class has name = {have}; it has to be {want}, as in {file}.", have=repr("other_one"),
                    want=repr("wrong_name"), file="plugin.json")]


@scene("plugins-failed-rows", "settings", "Plugins · added ones that didn't start")
def plugins_failed_rows(stack):
    win, settings, panel = plugins_context(stack, before=failing_plugins)
    nest(settings, *failing_texts())
    return settings


@scene("banner-plugins-failed", "states", "Plugins that didn't start")
def banner_plugins_failed(stack):
    win = loaded_window(stack, failing_plugins)
    return window_wide(win)


@scene("banner-plugins-failed-many", "states", "Plugins that didn't start · four or more")
def banner_plugins_failed_many(stack):
    def before():
        for name in ("one_card", "two_card", "three_card", "four_card"):
            write_plugin(name, {"class": "helper"})
            (plugin_loader().plugins_dir() / name / "__init__.py").write_text("helper = 1\n", encoding="utf-8")
            turn_on(name)
    return window_wide(loaded_window(stack, before))


@scene("plugins-dev", "settings", "Plugins · loaded from a development folder")
def plugins_dev(stack):
    import telescope.plugin_loader as loader
    folders = []

    def before():
        base = plugin_loader().plugins_dir().parent / "dev_plugins"
        base.mkdir(parents=True)
        for name, broken in (("dev_card", False), ("dev_broken", True)):
            path = base / name
            path.mkdir()
            (path / "plugin.json").write_text(
                '{"name": "%s", "title": "%s", "version": "0.1", "api": 1, "class": "Demo", "description": "Being written."}'
                % (name, name.replace("_", " ").title()), encoding="utf-8")
            if not broken:
                (path / "__init__.py").write_text(GOOD_PLUGIN.format(cls="Demo", name=name), encoding="utf-8")
            folders.append(path)

    def dev_folders():
        return list(folders)

    stack.enter_context(mock.patch.object(loader.dev_profile, "plugin_folders", dev_folders))
    win, settings, panel = plugins_context(stack, before=before)
    nest(settings, i18n.tr("It has no __init__.py."))
    return settings


@scene("plugins-changed-files", "settings", "Plugins · an added one whose files changed")
def plugins_changed_files(stack):
    def before():
        path = write_plugin("counter_card")
        turn_on("counter_card")
        with open(path / "__init__.py", "a", encoding="utf-8") as handle:
            handle.write("\n# edited\n")
    return plugins_scene(stack, before=before)


# ── Plugins turned off, and what the window says about them ──

def turned_off(*names):
    def before():
        for name in names:
            plugin_loader().set_builtin_off(name, True)
    return before


def off_titles(win, count):
    from telescope.i18n import tr
    return [tr(p.title or p.name) for p in win._builtin_plugins if p.name in count and not p.required]


def list_texts(count):
    """The ", " and "{items} and {last}" and_list joined a list of count titles with."""
    texts = []
    if count >= 2:
        texts.append(i18n.tr("{items} and {last}", items="A", last="B"))
    if count >= 3:
        texts.append(i18n.tr(", "))
    return texts


@scene("banner-plugins-off", "states", "A plugin turned off")
def banner_plugins_off(stack):
    win = loaded_window(stack, turned_off("monitoring"))
    return window_wide(win)


@scene("banner-plugins-off-several", "states", "Several plugins turned off")
def banner_plugins_off_several(stack):
    win = loaded_window(stack, turned_off("monitoring", "transforms", "presets"))
    nest(win, *list_texts(3))
    return window_wide(win)


@scene("banner-plugins-off-two", "states", "Two plugins turned off")
def banner_plugins_off_two(stack):
    win = loaded_window(stack, turned_off("monitoring", "transforms"))
    nest(win, *list_texts(2))
    return window_wide(win)


# ── Questions and warnings of the Plugins page ──

def panel_dialog(stack, act, ask=False, texts=(), **kwargs):
    """The box the panel opens when act(win, panel) calls one of its real handlers."""
    from PyQt6.QtWidgets import QCheckBox
    from telescope.widgets import plugins_panel
    held = hold_dialogs(stack)
    if ask:
        stack.enter_context(mock.patch.object(plugins_panel, "_ask", lambda *_a, **_k: True))
    win, settings, panel = plugins_context(stack, **kwargs)
    box = QCheckBox()
    stack.callback(box.deleteLater)
    act(win, panel, box)
    nest(held[-1], *texts)
    return shown(stack, held[-1])


def builtin(win, with_note):
    return next(p for p in win._builtin_plugins if not p.required and not p.optional and bool(p.when_off) == with_note)


@scene("dialog-plugin-off-note", "dialogs", "Plugins · switch off a built-in plugin that says what goes")
def plugin_off_with_note(stack):
    return panel_dialog(stack, lambda win, panel, box: panel._switch_builtin(builtin(win, True), False, box))


def one_added(**kwargs):
    def before():
        write_plugin("counter_card")
        turn_on("counter_card")
    return {"before": before, **kwargs}


@scene("dialog-plugin-added-on", "dialogs", "Plugins · switch on an added plugin")
def plugin_added_on(stack):
    def before():
        write_plugin("counter_card")
    return panel_dialog(stack, lambda win, panel, box: panel._switch_added(installed("counter_card"), True, box), before=before)


@scene("dialog-plugin-added-off", "dialogs", "Plugins · switch off an added plugin")
def plugin_added_off(stack):
    return panel_dialog(stack, lambda win, panel, box: panel._switch_added(installed("counter_card"), False, box), **one_added())


@scene("dialog-plugin-remove", "dialogs", "Plugins · remove an added plugin")
def plugin_remove(stack):
    return panel_dialog(stack, lambda win, panel, box: panel._remove(installed("counter_card")), **one_added())


@scene("dialog-plugin-remove-failed", "dialogs", "Plugins · an added plugin couldn't be removed")
def plugin_remove_failed(stack):
    def refuse(_item):
        raise OSError("[Errno 13] Permission denied")
    stack.enter_context(mock.patch.object(plugin_loader(), "remove", refuse))
    return panel_dialog(stack, lambda win, panel, box: panel._remove(installed("counter_card")), ask=True, **one_added())


@scene("dialog-plugin-download", "dialogs", "Plugins · download for an optional plugin")
def plugin_download(stack):
    return panel_dialog(stack, lambda win, panel, box: panel._switch_optional(
        next(p for p in win._builtin_plugins if p.needs_vision), True, box))


@scene("dialog-plugin-download-remove", "dialogs", "Plugins · remove the download")
def plugin_download_remove(stack):
    return panel_dialog(stack, lambda win, panel, box: panel._remove_pack(), download=fake_download_class()(ready=True))


@scene("dialog-plugin-download-remove-failed", "dialogs", "Plugins · the download couldn't be removed")
def plugin_download_remove_failed(stack):
    return panel_dialog(stack, lambda win, panel, box: panel._remove_pack(), ask=True,
                        download=fake_download_class()(ready=True, remove_error="[Errno 13] Permission denied"))


@scene("dialog-plugin-restart", "dialogs", "Plugins · restart while streaming")
def plugin_restart(stack):
    return panel_dialog(stack, lambda win, panel, box: panel._restart_now(), streaming_now=True)


@scene("dialog-plugin-restart-failed", "dialogs", "Plugins · Telescope couldn't restart")
def plugin_restart_failed(stack):
    def act(win, panel, box):
        panel._restart = lambda: False
        panel._restart_now()
    return panel_dialog(stack, act)


@scene("dialog-plugin-save-failed", "dialogs", "Plugins · a switch couldn't be saved")
def plugin_save_failed(stack):
    def act(win, panel, box):
        with mock.patch.object(plugin_loader(), "save_config", lambda _cfg: False):
            panel._all_on()
    return panel_dialog(stack, act, texts=[i18n.tr("Couldn't save the settings.")])


@scene("dialog-plugin-folder-failed", "dialogs", "Plugins · the plugins folder couldn't be made")
def plugin_folder_failed(stack):
    def refuse(*_args):
        raise OSError("[Errno 13] Permission denied")
    stack.enter_context(mock.patch.object(plugin_loader(), "make_plugins_dir", refuse))
    return panel_dialog(stack, lambda win, panel, box: panel._open_folder())


@scene("dialog-plugin-install-file", "dialogs", "Plugins · pick a plugin's .zip file")
def plugin_install_file(stack):
    from PyQt6.QtWidgets import QFileDialog
    held = []

    def get(parent, caption, _directory, name_filter, *_args, **_kwargs):
        dialog = QFileDialog(parent, caption, str(plugin_loader().make_plugins_dir()), name_filter)
        dialog.setOption(QFileDialog.Option.DontUseNativeDialog)
        dialog.setWindowTitle(caption)
        remember(dialog, "name filter", name_filter)
        held.append(dialog)
        return "", ""

    stack.enter_context(mock.patch.object(QFileDialog, "getOpenFileName", staticmethod(get)))
    win, settings, panel = plugins_context(stack)
    panel._add_zip()
    return shown(stack, held[-1])


# ── Installing a plugin from a .zip ──

def make_zip(name, entries, links=()):
    """A .zip of entries ({path in the archive: bytes}) in the profile's folder, plus symbolic links ({path: target})."""
    path = plugin_loader().plugins_dir().parent / f"{name}.zip"
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w") as archive:
        for entry, content in entries.items():
            archive.writestr(entry, content)
        for entry, target in dict(links).items():
            info = zipfile.ZipInfo(entry)
            info.external_attr = (0o120777 << 16)
            archive.writestr(info, target)
    return path


def manifest_of(name, **extra):
    import json
    return json.dumps({"name": name, "title": name.title(), "version": "1.0", "author": "Sam", "api": 1, "class": "Demo",
                       "description": "Adds a small card with a counter.", **extra})


def good_entries(name, **extra):
    return {f"{name}/plugin.json": manifest_of(name, **extra), f"{name}/__init__.py": GOOD_PLUGIN.format(cls="Demo", name=name)}


def install(stack, build, level="warning"):
    """The box the panel shows when a plugin's .zip is installed through its real handler."""
    held = hold_dialogs(stack)
    win, settings, panel = plugins_context(stack)
    archive = build()
    return win, panel, held, archive


def install_error(stack, build):
    win, panel, held, archive = install(stack, build)
    try:
        plugin_loader().install_zip(archive, panel._reserved)
        raise AssertionError("this archive was meant to be turned down")
    except ValueError as exc:
        text = exc.args[0]
    panel._install(str(archive))
    remember(held[-1], "text", text)
    return shown(stack, held[-1])


def bad_zip():
    path = plugin_loader().plugins_dir().parent / "notes.zip"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("not an archive", encoding="utf-8")
    return path


for _id, _title, _build in (
        ("install-not-a-zip", "isn't a .zip", bad_zip),
        ("install-outside", "a path outside the plugin", lambda: make_zip("outside", {"../escape.py": "x"})),
        ("install-link", "a link inside", lambda: make_zip("linked", good_entries("linked"), {"linked/shortcut": "/etc/passwd"})),
        ("install-two-folders", "two folders", lambda: make_zip("twin", {**good_entries("one_card"), **good_entries("two_card")})),
        ("install-bad-name", "a folder name that isn't allowed", lambda: make_zip("badname", {"Bad-Name/plugin.json": "{}"})),
        ("install-reserved", "a name that's built in", lambda: make_zip("reserved", good_entries("monitoring"))),
        ("install-too-big", "too many files", lambda: make_zip(
            "huge", {f"huge/part_{n}.txt": "" for n in range(501)}))):
    scene(f"dialog-plugin-{_id}", "dialogs", f"Plugins · installing a zip that {_title}")(
        lambda stack, build=_build: install_error(stack, build))


@scene("dialog-plugin-install-again", "dialogs", "Plugins · installing one that's already there")
def install_again(stack):
    win, panel, held, archive = install(stack, lambda: make_zip("counter_card", good_entries("counter_card")))
    plugin_loader().install_zip(archive, panel._reserved)
    panel._install(str(archive))
    return shown(stack, held[-1])


@scene("dialog-plugin-install-wont-load", "dialogs", "Plugins · installed, but it won't load")
def install_wont_load(stack):
    win, panel, held, archive = install(stack, lambda: make_zip(
        "no_class_card", {"no_class_card/plugin.json": manifest_of("no_class_card", **{"class": ""}),
                          "no_class_card/__init__.py": ""}))
    panel._install(str(archive))
    problem = installed("no_class_card").problem
    nest(held[-1], problem)
    return shown(stack, held[-1])


@scene("dialog-plugin-installed", "dialogs", "Plugins · installed")
def install_done(stack):
    win, panel, held, archive = install(stack, lambda: make_zip("counter_card", good_entries("counter_card")))
    panel._install(str(archive))
    return shown(stack, held[-1])


# ── The tray icon's tooltip ──

def tray_tooltip(stack, camera_on, mic):
    """The tooltip the tray icon gets for what streams, on an icon of its own since the system draws the tray."""
    from PyQt6.QtCore import Qt
    from PyQt6.QtWidgets import QApplication, QLabel
    win = live_window(stack, tray=True)
    tips = []
    original = win._tray.setToolTip
    win._tray.setToolTip = lambda text: (tips.append(text), original(text))
    streaming(win, phone_source(win))
    if not camera_on:
        mic_on(win)
        win.set_camera_on(False)
    if mic is not None:
        win._bus.mic_changed.emit(True, mic == "muted")
    win._show_tray_state()
    label = QLabel()
    label.setPixmap(win._tray.icon().pixmap(48, 48))
    label.setContentsMargins(40, 40, 40, 40)
    label.setToolTip(tips[-1])
    label.setAlignment(Qt.AlignmentFlag.AlignCenter)
    label.show()
    QApplication.processEvents()
    stack.callback(label.close)
    return label


for _id, _title, _camera, _mic in (
        ("tray-camera", "camera", True, None), ("tray-camera-mic", "camera and mic", True, "on"),
        ("tray-camera-muted", "camera, mic muted", True, "muted"), ("tray-mic", "mic, camera off", False, "on"),
        ("tray-mic-muted", "mic muted, camera off", False, "muted")):
    scene(_id, "states", f"Tray icon tooltip · {_title}")(lambda stack, c=_camera, m=_mic: tray_tooltip(stack, c, m))


# ── A plugin written for the old settings menu ──

@functools.cache
def legacy_plugin_class():
    from PyQt6.QtGui import QAction
    from telescope.plugin import TelescopePlugin

    class LegacyPlugin(TelescopePlugin):
        name = "legacy_demo"
        title = "Legacy demo"

        def create_menu_actions(self):
            return [QAction("Do something")]

    return LegacyPlugin


@scene("settings-legacy-plugin", "settings", "Settings · a plugin written for the old settings menu")
def settings_legacy_plugin(stack):
    from PyQt6.QtWidgets import QApplication
    from telescope.widgets.settings_window import _SIZE
    win = live_window(stack)
    win.register_plugin(legacy_plugin_class()())
    win.resize(1380, 900)
    win.open_settings("general")
    settings = win._settings_win
    QApplication.processEvents()
    grow(settings, _SIZE[0], _SIZE[1])
    return settings


# ── On Air sign ──────────────────────────────────────────────────────────────

def on_air_section(stack, setup=None):
    """The On Air page's section on its own, after setup(plugin)."""
    from scripts.translation_scenes_camera import on_its_own
    win = main_window(stack)
    win.open_settings()
    plugin = win._plugin("on_air")
    if setup is not None:
        setup(plugin)
    plugin._section.refresh()
    return on_its_own(stack, plugin._section)


@scene("on-air-problems", "states", "On Air · on air, with problems")
def on_air_problems(stack):
    def broken(plugin):
        class Taken(plugin._server_cls):
            def start(self):
                raise OSError(98, "Address already in use")
        plugin._debounce.state = True
        plugin.set_page(False)
        plugin._server_cls = Taken
        plugin.set_page(True)
        plugin.webhook_url = "homeassistant.local/api/webhook/office"
        plugin.webhook_method = "GET"
        plugin._on_webhook_done("test", "HTTP 404")
    return on_air_section(stack, broken)


@scene("on-air-test-sending", "states", "On Air · webhook test on its way")
def on_air_test_sending(stack):
    def sending(plugin):
        plugin.webhook_url = "http://homeassistant.local:8123/api/webhook/office"
        plugin._sender = _Held()
        plugin.send_test(True)
    return on_air_section(stack, sending)


@scene("on-air-test-sent", "states", "On Air · webhook test that worked")
def on_air_test_sent(stack):
    return on_air_section(stack, lambda plugin: plugin._on_webhook_done("test", ""))


@scene("on-air-test-no-address", "states", "On Air · webhook test without an address")
def on_air_test_no_address(stack):
    return on_air_section(stack, lambda plugin: plugin.send_test(False))


@scene("on-air-cryptography-missing", "states", "On Air · sign without the cryptography package")
def on_air_cryptography_missing(stack):
    from telescope.plugins import on_air as on_air_plugin

    def missing(plugin):
        stack.enter_context(mock.patch.object(on_air_plugin, "certificate_available", lambda: False))
        plugin.set_page(False)
        plugin.set_page(True)
    return on_air_section(stack, missing)


@scene("on-air-header-and-banner", "states", "On Air · on air, and the webhook didn't go through")
def on_air_header_and_banner(stack):
    win = live_window(stack)
    plugin = win._plugin("on_air")
    plugin._debounce.state = True
    plugin._refresh()
    plugin._on_webhook_done("state", "Connection refused")
    return window_wide(win)
