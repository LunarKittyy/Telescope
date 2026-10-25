"""Scenes for microphone, shortcuts: see translation_scenes.py for how scenes are registered."""

import io
import tempfile
import urllib.error
from pathlib import Path
from types import SimpleNamespace

from scripts.translation_scenes import (  # noqa: F401
    _MicBackend, bubble, capture_notifications, hold_dialogs, main_window, scene, shown, use_scratch_profile,
)


# ── Microphone card ──────────────────────────────────────────────────────────

class MicHost:
    """What the Microphone plugin asks the window: which stream the mic is on and which one the panels show."""

    def __init__(self, main=None):
        self.main = main
        self.saves = 0

    def schedule_save(self):
        self.saves += 1

    def main_stream(self):
        return self.main

    def focused_source_id(self):
        return "phone-1"

    def is_streaming_from(self, _source_id):
        return True

    def stream_count(self):
        return 1

    def make_main(self, _source_id):
        return True


class MicWorker:
    """Stands in for the audio worker: never opens a connection."""

    def __init__(self, url, auth, open_sink, on_status):
        self.url, self.auth, self.on_status = url, auth, on_status
        self.gain, self.muted, self.limit, self.auto_level = 1.0, False, True, False

    def take_level(self):
        return 0.0, 0.0, False

    def start(self):
        pass

    def stop(self):
        pass

    def hand_over(self, url):
        pass


class MicStub(_MicBackend):
    """A backend that is fine, or has a problem the card then shows."""

    def __init__(self, problem=None, prepare_error=""):
        self._problem, self._prepare_error = problem, prepare_error

    def problem(self):
        return self._problem

    def prepare(self):
        if isinstance(self._prepare_error, Exception):
            raise self._prepare_error
        return self._prepare_error

    def install(self):
        return ""


def track_field_texts(stack):
    """Texts the app shows are tagged by the preview's tr, but modules imported before it kept the plain one: give
    those the tagged one too. A text filled into another one's {field} is a text of its own on screen ("Mute:
    muted"), so it keeps its tag in the sentence."""
    import sys
    from unittest import mock
    from telescope import i18n
    from scripts.translation_scenes import Joined, parts_of
    inner = i18n.tr

    def translate(text, /, ctx="", **values):
        result = inner(text, ctx=ctx, **values)
        nested = [part for value in values.values() for part in parts_of(value)]
        return Joined(str(result), parts_of(result) + nested) if nested else result
    for name, module in list(sys.modules.items()):
        plain = getattr(module, "tr", None)
        if name.startswith("telescope.") and name != "telescope.i18n" and getattr(plain, "__module__", "") == "telescope.i18n":
            stack.enter_context(mock.patch.object(module, "tr", translate))
    stack.enter_context(mock.patch.object(i18n, "tr", translate))


def keep_tags_in_tooltips(stack):
    """A slider's tooltip is joined with "\\n".join, which drops the tag that says which text it shows: join it with + here."""
    from unittest import mock
    from PyQt6.QtWidgets import QSlider
    from telescope.i18n import tr
    from telescope.widgets.common import NoScrollSlider

    def set_tooltip(self, text):
        self._tip = text
        hint = tr(self.RESET_HINT) if self._default is not None else ""
        QSlider.setToolTip(self, text + "\n" + hint if hint else text)
    stack.enter_context(mock.patch.object(NoScrollSlider, "setToolTip", set_tooltip))


def mic_card(stack, backend=None, run_job=None, main=None, on=True, streaming=False):
    """The Microphone card on its own, in the state a user reaches by switching it on (and starting a stream)."""
    from PyQt6.QtWidgets import QApplication, QVBoxLayout, QWidget
    from telescope.plugin import EventBus
    from telescope.plugins.microphone import MicrophonePlugin
    keep_tags_in_tooltips(stack)
    track_field_texts(stack)
    host, bus = MicHost(main), EventBus()
    plugin = MicrophonePlugin(backend=backend or MicStub(), worker_cls=MicWorker, run_job=run_job or (lambda job: None))
    plugin.setup(host, bus)
    # Qt signals turn a text into a plain str and drop its tag, so the results of setup and install go straight in
    plugin._sig = SimpleNamespace(prepared=SimpleNamespace(emit=plugin._on_prepared),
                                  installed=SimpleNamespace(emit=plugin._on_installed),
                                  status=SimpleNamespace(emit=plugin._on_worker_status))
    panel = plugin.create_panel()
    stack.callback(plugin._meter_timer.stop)
    top = QWidget()
    top.setObjectName("translation_preview")
    layout = QVBoxLayout(top)
    layout.setContentsMargins(16, 16, 16, 16)
    layout.addWidget(panel)
    bus.phones_changed.emit(1)
    panel.setGraphicsEffect(None)
    panel.setEnabled(True)
    plugin.panel_top = top
    plugin.bus = bus
    if on:
        plugin.set_config({"enabled": True})
    if streaming:
        plugin.on_stream_start("http://192.168.1.20:8080/v1/stream", SimpleNamespace(base="http://192.168.1.20:8080/v1", auth=None))
    return plugin


def show_card(stack, plugin):
    from PyQt6.QtWidgets import QApplication
    top = plugin.panel_top
    top.resize(480, 400)
    top.show()
    QApplication.processEvents()
    top.resize(480, top.sizeHint().height())
    QApplication.processEvents()
    stack.callback(top.close)
    return top


def mic_scene(id, title, build, section_id="states"):
    scene(id, section_id, title, width=480)(lambda stack: show_card(stack, build(stack)))


mic_scene("mic-off", "Microphone · switched off", lambda stack: mic_card(stack, on=False))
mic_scene("mic-ready", "Microphone · on, waiting for a stream", lambda stack: mic_card(stack))
mic_scene("mic-browser", "Microphone · browser as the source", lambda stack: browser_mic(stack))


def browser_mic(stack):
    plugin = mic_card(stack)
    plugin.bus.source_selected.emit("browser:x")
    return plugin


def muted_mic(stack):
    plugin = mic_card(stack)
    plugin._mute_btn.setChecked(True)
    return plugin


mic_scene("mic-muted", "Microphone · muted", muted_mic)


def other_stream_mic(stack, on):
    plugin = mic_card(stack, main=("phone-2", "Galaxy S21"), on=on)
    plugin.bus.streams_changed.emit(2)
    return plugin


mic_scene("mic-elsewhere", "Microphone · on for another phone", lambda stack: other_stream_mic(stack, True))


def quiet_logs(stack):
    """The failures the scenes provoke are logged by the app; keep that out of the output."""
    import logging
    logging.disable(logging.CRITICAL)
    stack.callback(logging.disable, logging.NOTSET)


def linux_backend(stack, missing=()):
    """The real Linux backend, on a machine that lacks the tools named in missing."""
    from unittest import mock
    from telescope.platform import virtual_mic
    from telescope.plugins.microphone import LinuxMic
    stack.enter_context(mock.patch.object(virtual_mic, "linux_tools_missing", lambda *_a: list(missing)))
    return LinuxMic()


def windows_backend(stack, cable=False, opened=False, sound_device=True):
    """The real Windows backend, with or without the sounddevice package and VB-Cable."""
    from telescope.platform import virtual_mic
    from telescope.plugins.microphone import WindowsMic
    backend = WindowsMic()
    devices = [{"name": f"{virtual_mic.VB_CABLE_PLAYBACK} (VB-Audio Virtual Cable)", "max_output_channels": 2}] if cable else []
    backend._sd = SimpleNamespace(query_devices=lambda: devices) if sound_device else None
    backend.setup_opened = opened
    return backend


mic_scene("mic-linux-tools", "Microphone · pactl missing (Linux)",
          lambda stack: mic_card(stack, linux_backend(stack, ["pactl"])))
mic_scene("mic-win-no-sounddevice", "Microphone · sounddevice missing (Windows)",
          lambda stack: mic_card(stack, windows_backend(stack, sound_device=False)))
mic_scene("mic-win-no-cable", "Microphone · VB-Cable not installed (Windows)",
          lambda stack: mic_card(stack, windows_backend(stack)))
mic_scene("mic-win-cable-opened", "Microphone · VB-Cable setup was opened (Windows)",
          lambda stack: mic_card(stack, windows_backend(stack, opened=True)))
mic_scene("mic-setting-up", "Microphone · setting up the virtual microphone",
          lambda stack: mic_card(stack, streaming=True))


# ── Microphone setup and stream problems ─────────────────────────────────────

def pactl_answering(stack, answers):
    """The Linux backend setting up with a sound server that answers each pactl call from answers (ok, text)."""
    import functools
    from unittest import mock
    from telescope.platform import virtual_mic
    backend = linux_backend(stack)
    calls = iter(answers)
    setup = functools.partial(virtual_mic.linux_setup, run=lambda *_a, **_k: next(calls, (False, "")),
                              fifo=str(Path(tempfile.gettempdir()) / "telescope-translation-mic.fifo"))
    stack.enter_context(mock.patch.object(virtual_mic, "linux_setup", setup))
    return backend


def setup_problem(stack, backend):
    quiet_logs(stack)
    return mic_card(stack, backend, run_job=lambda job: job(), streaming=True)


def sound_server_silent(stack):
    return setup_problem(stack, pactl_answering(stack, [(False, "")]))


def sound_server_refuses(stack):
    return setup_problem(stack, pactl_answering(stack, [(True, ""), (False, "Failure: Module initialization failed"),
                                                        (False, "Failure: Module initialization failed")]))


def setup_crashes(stack):
    return setup_problem(stack, MicStub(prepare_error=RuntimeError("pactl is not responding")))


mic_scene("mic-card-setup-no-server", "Microphone · the sound server didn't answer", sound_server_silent)
mic_scene("mic-card-setup-refused", "Microphone · the virtual microphone couldn't be created", sound_server_refuses)
mic_scene("mic-card-setup-crash", "Microphone · setup failed with an error", setup_crashes)


def connecting(stack):
    return mic_card(stack, run_job=lambda job: job(), streaming=True)


mic_scene("mic-connecting", "Microphone · connecting to the phone", connecting)


class _Stream:
    """A phone's audio reply that ends at once."""

    def read1(self, _size):
        return b""

    def close(self):
        pass


def phone_answer(opener):
    """What the audio worker reports when the phone's microphone connection ends this way (its own code runs)."""
    from telescope import audio
    from telescope.pinned_https import PhoneAuth
    worker = audio.AudioWorker("http://192.168.1.20:8080/v1/audio", PhoneAuth("token"), lambda: None, lambda *_a: None,
                               opener=opener)
    return worker._read_once()


def refused(code, body=b""):
    def opener(_request, timeout):
        raise urllib.error.HTTPError("http://192.168.1.20:8080/v1/audio", code, "refused", {}, io.BytesIO(body))
    return opener


def unreachable(_request, timeout):
    raise OSError("No route to host")


def output_answer(sink):
    """What the audio worker reports when the virtual microphone breaks (its own code runs)."""
    from unittest import mock
    from telescope import audio
    from telescope.pinned_https import PhoneAuth
    worker = audio.AudioWorker("http://192.168.1.20:8080/v1/audio", PhoneAuth("token"), sink, lambda *_a: None)
    with mock.patch.object(audio.logger, "exception"):
        return worker._write_once(False)


def broken_output(stack, error=None):
    def sink():
        if error is not None:
            raise error

        class Dead:
            def write(self, _data):
                raise BrokenPipeError("reader went away")

            def close(self):
                pass
        return Dead()
    return sink


def stream_trouble(stack, text):
    """The card with a stream running, then the worker reporting this problem."""
    plugin = mic_card(stack, run_job=lambda job: job(), streaming=True)
    plugin._on_worker_status("err", text)
    return plugin


def phone_says(code, english, status=409):
    import json
    return refused(status, json.dumps({"code": code, "error": english}).encode())


STREAM_TROUBLES = (
    ("phone-unreachable", "the phone can't be reached", lambda stack: phone_answer(unreachable)),
    ("phone-stopped", "the phone's microphone stopped", lambda stack: phone_answer(lambda *_a, **_k: _Stream())),
    ("phone-no-mic", "the phone has no microphone", lambda stack: phone_answer(phone_says("mic_none", "No microphone"))),
    ("phone-not-streaming", "the phone isn't streaming",
     lambda stack: phone_answer(phone_says("mic_not_streaming", "Not streaming"))),
    ("phone-permission", "the phone's permission is missing",
     lambda stack: phone_answer(phone_says("mic_permission", "Microphone permission is missing"))),
    ("phone-open-app", "the phone's app needs opening",
     lambda stack: phone_answer(phone_says("mic_open_app", "Open the app"))),
    ("phone-in-use", "the phone's microphone is in use", lambda stack: phone_answer(phone_says("mic_in_use", "In use"))),
    ("phone-refused", "the phone refused it", lambda stack: phone_answer(refused(500, b"not json"))),
    ("output-cant-open", "the virtual microphone can't be opened",
     lambda stack: output_answer(broken_output(stack, OSError("Device or resource busy")))),
    ("output-stopped", "the virtual microphone stopped", lambda stack: output_answer(broken_output(stack))),
)


def stream_trouble_scene(slug, title, text_of):
    def build(stack):
        quiet_logs(stack)
        text = text_of(stack)
        return stream_trouble(stack, text)
    mic_scene(f"mic-stream-{slug}", f"Microphone · {title}", build)


for _slug, _title, _text in STREAM_TROUBLES:
    stream_trouble_scene(_slug, _title, _text)


# ── Installing VB-Cable (Windows) ────────────────────────────────────────────

class _Pack:
    """A download that hands over these bytes."""

    def __init__(self, data):
        self._data = io.BytesIO(data)

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        return False

    def read(self, size=-1):
        return self._data.read(size)


def zip_of(names):
    import zipfile
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as pack:
        for name in names:
            pack.writestr(name, b"x")
    return out.getvalue()


def install_problem(stack, download=None, setup=None, patches=(), bare=False):
    """The card after Install VB-Cable was clicked and the real download and setup code ran against this situation:
    download is what the site answers, setup what Windows answers when the setup is started."""
    import functools
    from unittest import mock
    from telescope.platform import virtual_mic
    quiet_logs(stack)
    folder = stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-translation-cable-"))
    fetch = functools.partial(virtual_mic.download_vb_cable, Path(folder), download or (lambda *_a, **_k: None))
    run = functools.partial(virtual_mic.run_vb_cable_setup, start=setup)
    if bare:  # the download works, so the next step is checking what was downloaded and starting it
        exe = Path(folder) / virtual_mic.VB_CABLE_SETUP
        exe.write_bytes(b"not VB-Audio's setup")
        fetch = lambda: (exe, "")  # noqa: E731
    stack.enter_context(mock.patch.object(virtual_mic, "download_vb_cable", fetch))
    stack.enter_context(mock.patch.object(virtual_mic, "run_vb_cable_setup", run))
    for target, name, value in patches:
        stack.enter_context(mock.patch.object(target, name, value))
    backend = windows_backend(stack)
    plugin = mic_card(stack, backend, run_job=lambda job: job())
    plugin._confirm_install = lambda: True  # the question has its own scene
    plugin._action_btn.click()
    return plugin


def sha_ok(stack):
    from telescope.platform import virtual_mic
    return [(virtual_mic, "_sha256", lambda _path: virtual_mic.VB_CABLE_PACK_SHA256)]


def windows_error(code):
    import ssl

    def start(*_a, **_k):
        error = OSError("blocked")
        if code:
            error.winerror = code
        raise error
    return start


def cert_failure(*_a, **_k):
    import ssl
    raise urllib.error.URLError(ssl.SSLCertVerificationError("self-signed certificate"))


def site_refuses(*_a, **_k):
    raise urllib.error.HTTPError("https://download.vb-audio.com/", 403, "Forbidden", {}, io.BytesIO(b""))


def site_offline(*_a, **_k):
    raise urllib.error.URLError(OSError("Network is unreachable"))


def site_breaks(*_a, **_k):
    raise ConnectionResetError("reset")


def downloading(stack):
    plugin = mic_card(stack, windows_backend(stack), run_job=lambda job: None)
    plugin._confirm_install = lambda: True
    plugin._action_btn.click()
    return plugin


def installer_crashes(stack):
    quiet_logs(stack)
    backend = windows_backend(stack)

    def install():
        raise OSError("Access is denied")
    backend.install = install
    plugin = mic_card(stack, backend, run_job=lambda job: job())
    plugin._confirm_install = lambda: True
    plugin._action_btn.click()
    return plugin


def changed_pack(stack):
    return install_problem(stack, download=lambda *_a, **_k: _Pack(b"not the pack"))


def not_a_zip(stack):
    from telescope.platform import virtual_mic
    return install_problem(stack, download=lambda *_a, **_k: _Pack(b"not a zip"), patches=sha_ok(stack))


def no_setup_in_pack(stack):
    return install_problem(stack, download=lambda *_a, **_k: _Pack(zip_of(["readme.txt"])), patches=sha_ok(stack))


def setup_tampered(stack):
    return install_problem(stack, bare=True)


def setup_declined(stack):
    from telescope.platform import virtual_mic
    return install_problem(stack, bare=True, setup=windows_error(1223), patches=[
        (virtual_mic, "_sha256", lambda _path: virtual_mic.VB_CABLE_SETUP_SHA256),
        (virtual_mic, "_folder_is_the_pack", lambda _folder: True)])


def setup_wont_open(stack):
    from telescope.platform import virtual_mic
    return install_problem(stack, bare=True, setup=windows_error(0), patches=[
        (virtual_mic, "_sha256", lambda _path: virtual_mic.VB_CABLE_SETUP_SHA256),
        (virtual_mic, "_folder_is_the_pack", lambda _folder: True)])


mic_scene("mic-cable-downloading", "Microphone · VB-Cable is downloading", downloading)
mic_scene("mic-cable-site-refuses", "Microphone · VB-Cable: the site said no",
          lambda stack: install_problem(stack, download=site_refuses))
mic_scene("mic-cable-certificate", "Microphone · VB-Cable: certificate couldn't be checked",
          lambda stack: install_problem(stack, download=cert_failure))
mic_scene("mic-cable-offline", "Microphone · VB-Cable: offline", lambda stack: install_problem(stack, download=site_offline))
mic_scene("mic-cable-download-failed", "Microphone · VB-Cable: the download failed",
          lambda stack: install_problem(stack, download=site_breaks))
mic_scene("mic-cable-changed", "Microphone · VB-Cable: the download changed", changed_pack)
mic_scene("mic-cable-unpack", "Microphone · VB-Cable: couldn't unpack", not_a_zip)
mic_scene("mic-cable-no-setup", "Microphone · VB-Cable: no setup in the pack", no_setup_in_pack)
mic_scene("mic-cable-tampered", "Microphone · VB-Cable: the setup changed after the check", setup_tampered)
mic_scene("mic-cable-declined", "Microphone · VB-Cable: Windows asked and was told no", setup_declined)
mic_scene("mic-cable-wont-open", "Microphone · VB-Cable: the setup wouldn't open", setup_wont_open)
mic_scene("mic-cable-crash", "Microphone · VB-Cable: install failed with an error", installer_crashes)


# ── Keyboard shortcuts ───────────────────────────────────────────────────────

def shortcuts_plugin(stack):
    """The window with its real shortcuts plugin, started, and the plugin."""
    from unittest import mock
    from telescope.shortcuts import Param
    track_field_texts(stack)

    def options(self):  # the app's str() of each label drops its tag
        raw = self.choices() if callable(self.choices) else self.choices
        return list(raw)
    stack.enter_context(mock.patch.object(Param, "options", options))
    win = main_window(stack)
    return win, next(p for p in win._plugins if p.name == "shortcuts")


def binding(keys, action, params=None, everywhere=True):
    from telescope.shortcuts import new_binding_id
    return {"id": new_binding_id(), "keys": keys, "action": action, "params": params or {}, "global": everywhere}


def use_backend(plugin, backend):
    """The plugin hands its global keys to backend, as start() does with the one this system makes."""
    plugin._backend = backend
    if backend is not None:
        backend.changed.connect(plugin._refresh_section)
    plugin._last_global = None


def set_bindings(plugin, bindings, notify=False):
    plugin.set_config({"bindings": bindings, "notify": notify})


class QuietPortal:
    """The desktop portal's backend as it is once the portal answered, without talking to D-Bus."""

    def __new__(cls, state="ready", why=""):
        from telescope.platform.hotkeys_portal import PortalHotkeys

        class Portal(PortalHotkeys):
            def _run(self):
                pass
        portal = Portal("telescope-translation")
        portal._state, portal._why = state, why
        return portal


def portal_answers(plugin, answers):
    """What the portal says about each binding, in order: a desktop's reply for it, or None before it was answered."""
    from telescope.platform.hotkeys_portal import PortalHotkeys
    backend = plugin.backend
    for b, reply in zip(plugin.bindings, answers):
        if reply != "waiting":
            backend._statuses[b["id"]] = PortalHotkeys._status_of(reply)


class WindowsKeys:
    """What Windows lets an app claim: every key but those in taken (virtual-key codes), and punctuation that the
    keyboard layout can't type."""

    def __init__(self, taken=()):
        self.taken = set(taken)

    def VkKeyScanW(self, char):
        return -1 if char in ",;" else ord(char.upper())

    def RegisterHotKey(self, _window, _number, _flags, vk):
        return vk not in self.taken

    def UnregisterHotKey(self, _window, _number):
        return True


def windows_hotkeys(taken=()):
    from telescope.platform.hotkeys_windows import WindowsHotkeys
    return WindowsHotkeys(user32=WindowsKeys(taken), install_filter=False)


def shortcuts_page(stack, prepare):
    """Settings, Keyboard shortcuts, after prepare(window, plugin) set the bindings and the system's answers up."""
    from PyQt6.QtWidgets import QApplication
    from scripts.translation_scenes import grow
    from telescope.widgets.settings_window import _SIZE
    win, plugin = shortcuts_plugin(stack)
    win.resize(1380, 900)
    prepare(win, plugin)
    win.open_settings()
    settings = win._settings_win
    settings.show_tab("shortcuts")
    QApplication.processEvents()
    grow(settings, _SIZE[0], _SIZE[1])
    return settings


MUTE = "microphone.mute"
GAIN = "microphone.gain"
SOME_KINDS = (
    ("Ctrl+Alt+M", MUTE, {"mode": "toggle"}), ("Ctrl+Alt+P", MUTE, {"mode": "hold"}),
    ("Ctrl+Alt+Up", GAIN, {"op": "up", "amount": 3.0}), ("Ctrl+Alt+Down", GAIN, {"op": "down", "amount": 3.0}),
    ("Ctrl+Alt+0", GAIN, {"op": "reset"}), ("Ctrl+Alt+7", GAIN, {"op": "set", "value": -6.0}),
    ("Ctrl+Alt+I", "camera.iso", {"op": "up", "stops": 1.0}), ("Ctrl+Alt+O", "camera.iso", {"op": "down", "stops": 0.5}),
    ("Ctrl+Alt+L", "camera.lens", {"op": "next"}), ("Ctrl+Alt+K", "camera.lens", {"op": "previous"}),
    ("Ctrl+Alt+S", "streaming.start_stop", {"mode": "toggle"}), ("Ctrl+Alt+C", "streaming.camera", {"mode": "off"}),
    ("Ctrl+Alt+H", "window.show_hide", {}),
)


def kinds_of_shortcut(win, plugin):
    set_bindings(plugin, [binding(keys, action, params, everywhere=False) for keys, action, params in SOME_KINDS])


def in_telescope_only(win, plugin):
    use_backend(plugin, None)
    set_bindings(plugin, [binding("Ctrl+Alt+M", MUTE, {"mode": "toggle"}, everywhere=False),
                          binding("Ctrl+Shift+S", "streaming.start_stop", {"mode": "toggle"}),
                          binding("Ctrl+Alt+M", "camera.torch", {"mode": "toggle"}),
                          binding("Ctrl+Alt+G", "gone.thing", {})], notify=True)


def with_portal(state, answers, why=""):
    def prepare(win, plugin):
        use_backend(plugin, QuietPortal(state, why))
        set_bindings(plugin, [binding("Ctrl+Alt+M", MUTE, {"mode": "toggle"}),
                              binding("Ctrl+Alt+S", "streaming.start_stop", {"mode": "toggle"}),
                              binding("Ctrl+Alt+C", "streaming.camera", {"mode": "toggle"}),
                              binding("Ctrl+Alt+H", "window.show_hide", {})])
        portal_answers(plugin, answers)
    return prepare


def with_windows(win, plugin):
    use_backend(plugin, windows_hotkeys(taken=[0x74]))
    set_bindings(plugin, [binding("Ctrl+Alt+M", MUTE, {"mode": "toggle"}),
                          binding("Ctrl+Shift+F5", "streaming.start_stop", {"mode": "toggle"}),
                          binding("Ctrl+Alt+,", "streaming.camera", {"mode": "toggle"}),
                          binding("Ctrl+Alt+H", "window.show_hide", {}, everywhere=False)])


def page_scene(id, title, prepare, after=None):
    def build(stack):
        settings = shortcuts_page(stack, prepare)
        if after is not None:
            after(stack, settings)
        return settings
    scene(id, "settings", f"Settings · Keyboard shortcuts · {title}")(build)


page_scene("settings-shortcuts-kinds", "what each kind of shortcut does", kinds_of_shortcut)
page_scene("settings-shortcuts-telescope", "inside Telescope and clashes", in_telescope_only)
page_scene("settings-shortcuts-portal", "desktop portal, confirmed and not",
           with_portal("ready", [{"trigger_description": ("s", "Ctrl+Alt+M")}, None, {}, "waiting"]))
page_scene("settings-shortcuts-portal-asking", "desktop portal, waiting for the dialog",
           with_portal("asking", ["waiting"] * 4))
page_scene("settings-shortcuts-portal-unavailable", "desktop portal, not available",
           with_portal("unavailable", ["waiting"] * 4))
page_scene("settings-shortcuts-portal-failed", "desktop portal, failed with a reason",
           with_portal("unavailable", ["waiting"] * 4, why="no session"))
page_scene("settings-shortcuts-windows", "Windows", with_windows)


# ── The shortcut editor ──────────────────────────────────────────────────────

def editor_for(stack, keys="", action="", params=None, everywhere=True, others=(), after=None):
    """The shortcut editor open on this binding, with the bindings in others already saved."""
    from telescope.plugins.shortcuts import BindingEditor
    win, plugin = shortcuts_plugin(stack)
    if others:
        set_bindings(plugin, [binding(*other) for other in others])
    editor = BindingEditor(plugin, binding(keys, action, params, everywhere))
    if after is not None:
        after(editor)
    return shown(stack, editor)


def editor_scene(slug, title, **kwargs):
    scene(f"dialog-shortcut-{slug}", "dialogs", f"Keyboard shortcut editor · {title}")(
        lambda stack: editor_for(stack, **kwargs))


def type_search(text):
    return lambda editor: editor._search.setText(text)


def press_only_ctrl(editor):
    from PyQt6.QtCore import Qt
    from PyQt6.QtGui import QKeySequence
    editor._keys_edit.setKeySequence(QKeySequence(Qt.Key.Key_Control))


def pick_mode(mode):
    def pick(editor):
        box = editor._param_widgets["mode"][1]
        box.setCurrentIndex(box.findData(mode))
    return pick


editor_scene("no-match", "nothing found", after=type_search("zzzz"))
editor_scene("modifier", "only a modifier pressed", action=MUTE, params={"mode": "toggle"}, after=press_only_ctrl)
editor_scene("clash", "the key is taken", keys="Ctrl+Alt+M", action="camera.torch", params={"mode": "toggle"},
             others=[("Ctrl+Alt+M", MUTE, {"mode": "toggle"})])
editor_scene("plain-key", "a plain key everywhere", keys="M", action=MUTE, params={"mode": "toggle"})
editor_scene("mute-hold", "mute while held", keys="Ctrl+Alt+P", action=MUTE, params={"mode": "hold"})
editor_scene("gain-up", "gain up", keys="Ctrl+Alt+Up", action=GAIN, params={"op": "up", "amount": 3.0})
editor_scene("gain-set", "gain to a value", keys="Ctrl+Alt+7", action=GAIN, params={"op": "set", "value": -6.0})
editor_scene("iso-up", "ISO in stops", keys="Ctrl+Alt+I", action="camera.iso", params={"op": "up", "stops": 1.0})
editor_scene("iso-set", "ISO to a value", keys="Ctrl+Alt+J", action="camera.iso", params={"op": "set", "value": 400.0})
editor_scene("lens-next", "next lens", keys="Ctrl+Alt+L", action="camera.lens", params={"op": "next"})
editor_scene("lens-pick", "a lens that was saved and is gone now", keys="Ctrl+Alt+K", action="camera.lens", params={"op": "set", "option": "Main"})
editor_scene("unknown", "a control this version doesn't have", keys="Ctrl+Alt+G", action="gone.thing")


# ── Menus ────────────────────────────────────────────────────────────────────

def hold_menus(stack):
    """Right-click menus come up and stay up instead of waiting for a choice; returns the list they're added to."""
    from unittest import mock
    from PyQt6.QtCore import QPoint
    from PyQt6.QtWidgets import QMenu
    held = []

    def keep(menu, *_args):
        menu.popup(QPoint(20, 20))
        held.append(menu)
        stack.callback(menu.close)

    stack.enter_context(mock.patch.object(QMenu, "exec", keep))
    return held


def control_menu(stack, action_id, bindings=()):
    """What right-clicking the control of this action offers."""
    from PyQt6.QtCore import QPoint
    held = hold_menus(stack)
    win, plugin = shortcuts_plugin(stack)
    set_bindings(plugin, [binding(*b) for b in bindings])
    widget = plugin.action(action_id).controls()[0]
    plugin.control_menu(widget, action_id, QPoint(2, 2))
    return shown(stack, held[-1])


def row_menu(stack):
    """What right-clicking a shortcut in Settings offers."""
    from PyQt6.QtCore import QPoint
    held = hold_menus(stack)
    win, plugin = shortcuts_plugin(stack)
    set_bindings(plugin, [binding("Ctrl+Alt+M", MUTE, {"mode": "toggle"})])
    win.open_settings()
    plugin._section._row_menu(plugin.bindings[0]["id"], QPoint(20, 20))
    return shown(stack, held[-1])


scene("menu-control-mute", "dialogs", "Right-click on Mute", width=0)(
    lambda stack: control_menu(stack, MUTE, [("Ctrl+Alt+M", MUTE, {"mode": "toggle"}),
                                             ("Ctrl+Alt+P", MUTE, {"mode": "hold"})]))
scene("menu-control-pan", "dialogs", "Right-click on a control with two shortcuts")(
    lambda stack: control_menu(stack, "transforms.pan_x"))
scene("menu-shortcut-row", "dialogs", "Right-click on a shortcut in Settings")(row_menu)


# ── Shortcut notifications and replies ───────────────────────────────────────

def enable_mic(win):
    plugin = next(p for p in win._plugins if p.name == "microphone")
    plugin.set_config({**plugin.get_config(), "enabled": True})
    return plugin


def streaming_now(win, running=True):
    win.is_streaming = lambda: running
    win.is_starting = lambda: False
    win.start_stream = lambda **_kwargs: None
    win.stop_all_streams = lambda: None


def camera_state(win, on):
    state = {"on": on}
    win.is_camera_on = lambda: state["on"]
    win.can_turn_camera_off = lambda: (True, "")
    win.set_camera_on = lambda value: state.update(on=value)


def window_state(win, shown_now):
    win.toggle_window = lambda: shown_now


def a_shortcut_runs(stack, action_id, params, prepare=None, everywhere=True):
    """Press a global shortcut of this action while Telescope is in the background; returns the plugin's answer and
    the notifications it sent, drawn as the desktop shows them."""
    from unittest import mock
    from PyQt6.QtWidgets import QApplication
    sent = capture_notifications(stack)
    stack.enter_context(mock.patch.object(QApplication, "activeWindow", staticmethod(lambda: None)))
    win, plugin = shortcuts_plugin(stack)
    if prepare is not None:
        prepare(win)
    one = binding("Ctrl+Alt+X", action_id, params, everywhere)
    set_bindings(plugin, [one], notify=True)
    said = plugin.press(one["id"], "global")
    return said, sent


def notice_scene(slug, title, action_id, params, prepare=None):
    def build(stack):
        said, sent = a_shortcut_runs(stack, action_id, params, prepare)
        if not sent:
            raise RuntimeError(f"nothing was sent for {title}: the action said {said!r}")
        return bubble(stack, sent)
    scene(f"notify-shortcut-{slug}", "notifications", f"Notification · shortcut · {title}")(build)


def mic_ready_then(*steps):
    def prepare(win):
        mic = enable_mic(win)
        for step in steps:
            step(win, mic)
    return prepare


notice_scene("stream-start", "streaming starts", "streaming.start_stop", {"mode": "toggle"},
             lambda win: streaming_now(win, False))
notice_scene("stream-already", "already streaming", "streaming.start_stop", {"mode": "start"},
             lambda win: streaming_now(win, True))
notice_scene("stream-stopped", "streaming stopped", "streaming.start_stop", {"mode": "stop"},
             lambda win: streaming_now(win, True))
notice_scene("stream-not", "not streaming", "streaming.start_stop", {"mode": "stop"},
             lambda win: streaming_now(win, False))
notice_scene("camera-off", "phone camera off", "streaming.camera", {"mode": "off"}, lambda win: camera_state(win, True))
notice_scene("camera-on", "phone camera on", "streaming.camera", {"mode": "on"}, lambda win: camera_state(win, False))
notice_scene("window-hidden", "Telescope hidden", "window.show_hide", {}, lambda win: window_state(win, False))
notice_scene("mute", "microphone muted", MUTE, {"mode": "on"}, mic_ready_then())
notice_scene("unmute", "microphone unmuted", MUTE, {"mode": "off"},
             mic_ready_then(lambda win, mic: mic._mute_btn.setChecked(True)))
notice_scene("phone-mic", "phone mic switched off", "microphone.on", {"mode": "off"}, mic_ready_then())
notice_scene("gain", "gain changed", GAIN, {"op": "up", "amount": 3.0}, mic_ready_then())


# ── What `telescope --action` prints ─────────────────────────────────────────

def terminal(stack, command, reply):
    """A terminal with the command typed and the reply printed under it."""
    from PyQt6.QtWidgets import QApplication, QFrame, QLabel, QVBoxLayout
    top = QFrame()
    top.setObjectName("terminal")
    top.setStyleSheet("QFrame#terminal { background: #16161a; } QLabel { background: transparent; color: #d6d6dc; "
                      "font-family: monospace; font-size: 13px; }")
    column = QVBoxLayout(top)
    column.setContentsMargins(18, 16, 18, 16)
    column.setSpacing(4)
    typed = QLabel(f"$ {command}")
    column.addWidget(typed)
    output = QLabel(reply)
    output.setWordWrap(True)
    column.addWidget(output)
    top.setFixedWidth(620)
    top.show()
    top.adjustSize()
    QApplication.processEvents()
    stack.callback(top.close)
    return top


def command_reply(stack, request, prepare=None):
    """The reply the running Telescope gives a `telescope --action` request, under the command that asked."""
    win, plugin = shortcuts_plugin(stack)
    if prepare is not None:
        prepare(win)
    reply = plugin.handle_remote(request)
    words = [request["action"], *[f"{k}={v}" for k, v in request.get("params", {}).items()]]
    return terminal(stack, "telescope --action " + " ".join(words), reply["text"])


def command_scene(slug, title, request, prepare=None):
    scene(f"command-{slug}", "states", f"Command line · {title}")(lambda stack: command_reply(stack, request, prepare))


command_scene("unknown-action", "no such action", {"action": "streaming.nothing"})
command_scene("unknown-setting", "no such setting", {"action": MUTE, "params": {"speed": "1"}})
command_scene("not-a-number", "a number was expected", {"action": GAIN, "params": {"op": "up", "amount": "loud"}})
command_scene("wrong-choice", "one of the choices was expected", {"action": MUTE, "params": {"mode": "sideways"}})
command_scene("no-choices", "a choice with nothing to pick", {"action": "presets.apply", "params": {"name": "Desk"}})
command_scene("hold", "hold has no command", {"action": MUTE, "params": {"mode": "hold"}})
command_scene("missing-value", "a value is missing", {"action": GAIN, "params": {"op": "set"}})
command_scene("unavailable", "the control is off", {"action": MUTE, "params": {"mode": "toggle"}})
command_scene("shown", "Telescope shown", {"action": "window.show_hide"}, lambda win: window_state(win, True))
command_scene("muted", "microphone muted", {"action": MUTE, "params": {"mode": "on"}}, lambda win: enable_mic(win))
command_scene("camera-toggle", "phone camera switched", {"action": "streaming.camera", "params": {"mode": "off"}},
              lambda win: camera_state(win, True))


# ── Microphone in the main window and the tray ───────────────────────────────

def main_with_mic(stack, width_name):
    import telescope.app as app_module
    from scripts.translation_scenes import grow
    track_field_texts(stack)
    win = main_window(stack)
    enable_mic(win)
    grow(win, getattr(app_module, width_name), 900)
    return win


scene("main-microphone", "main", "Main window · microphone switched on")(lambda stack: main_with_mic(stack, "_WIDTH_THREE_COL"))


def tray_with_mic(stack):
    from PyQt6.QtCore import QPoint
    track_field_texts(stack)
    win = main_window(stack, tray=True)
    enable_mic(win)
    win._plugin("microphone")._mute_btn.setChecked(True)
    menu = win._tray.contextMenu()
    menu.popup(QPoint(20, 20))
    stack.callback(menu.close)
    return shown(stack, menu)


scene("menu-tray-microphone", "dialogs", "Tray menu · microphone muted")(tray_with_mic)
