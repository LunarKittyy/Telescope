"""Streaming by itself when the phone is ready, and opening at sign-in."""

import sys
import types
from pathlib import Path

import pytest

import telescope.platform.autostart as autostart
import telescope.plugins.startup as startup_module
from telescope.plugin import EventBus
from telescope.plugins.startup import StartupPlugin


class _Host:
    def __init__(self):
        self.streaming = False
        self.starting = False
        self.starts = []
        self.stops = 0
        self.keep = None
        self.saves = 0
        self.issues = {}

    def is_streaming(self):
        return self.streaming

    def is_starting(self):
        return self.starting

    def start_stream(self, interactive=True):
        self.starts.append(interactive)

    def stop_stream(self):
        self.stops += 1

    def set_keep_in_tray(self, keep):
        self.keep = keep

    def schedule_save(self):
        self.saves += 1

    def show_issue(self, key, issue):
        self.issues[key] = issue

    def clear_issue(self, key=None):
        self.issues.pop(key, None)


@pytest.fixture
def env(qapp):
    host, bus = _Host(), EventBus()
    plugin = StartupPlugin()
    plugin.setup(host, bus)
    return plugin, host, bus


def test_off_by_default_and_never_starts(env):
    plugin, host, bus = env
    bus.phone_ready.emit("p1", True)
    assert host.starts == []


def test_starts_once_per_arrival_without_asking(env):
    plugin, host, bus = env
    plugin.set_auto_stream(True)
    assert host.keep is True
    bus.phone_ready.emit("p1", True)
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False]
    bus.phone_ready.emit("p1", False)   # phone went away
    bus.phone_ready.emit("p1", True)    # and came back
    assert host.starts == [False, False]


def test_stop_sticks_until_the_phone_goes_away(env):
    plugin, host, bus = env
    plugin.set_auto_stream(True)
    bus.stream_stopped.emit()
    bus.phone_ready.emit("p1", True)
    assert host.starts == []
    bus.phone_ready.emit("p1", False)
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False]


def test_switching_phones_is_a_new_arrival(env):
    plugin, host, bus = env
    plugin.set_auto_stream(True)
    bus.phone_ready.emit("p1", True)
    bus.phone_ready.emit("p2", True)
    assert host.starts == [False, False]


def test_not_while_streaming(env):
    plugin, host, bus = env
    plugin.set_auto_stream(True)
    host.streaming = True
    bus.phone_ready.emit("p1", True)
    assert host.starts == []


def test_config_round_trip(env):
    plugin, host, _bus = env
    plugin.set_config({"auto_stream": True})
    assert plugin.get_config() == {"auto_stream": True, "watch_stream": False} and host.keep is True
    plugin.set_config({"auto_stream": "yes", "watch_stream": True})
    assert plugin.auto_stream is False and plugin.watch_stream is True and host.keep is True
    plugin.set_config({})
    assert host.keep is False


def test_menu_actions_reflect_and_change_the_settings(env, monkeypatch):
    plugin, host, _bus = env
    calls = []
    monkeypatch.setattr(startup_module.autostart, "is_enabled", lambda: False)
    monkeypatch.setattr(startup_module.autostart, "enable", lambda: calls.append("on") or (True, ""))
    monkeypatch.setattr(startup_module.autostart, "disable", lambda: calls.append("off") or (False, "denied"))
    divider, stream, sign_in, watch = plugin.create_menu_actions()
    assert divider.isSeparator()
    assert not stream.isChecked() and not sign_in.isChecked() and not watch.isChecked()
    watch.setChecked(True)
    assert plugin.watch_stream is True and host.keep is True
    stream.setChecked(True)
    assert plugin.auto_stream is True
    sign_in.setChecked(True)
    sign_in.setChecked(False)
    assert calls == ["on", "off"]
    assert host.issues["startup"].title == "denied"


# ── Streaming while an app uses the camera ─────────────────────────────────────

@pytest.fixture
def watching(env):
    plugin, host, bus = env
    plugin.set_watch_stream(True)
    bus.phone_ready.emit("p1", True)
    return plugin, host, bus


def _stream_runs(host, bus):
    host.streaming = True
    bus.stream_started.emit("url")


def _stream_ends(host, bus):
    host.streaming = False
    bus.stream_stopped.emit()


def test_watching_is_off_by_default(env):
    plugin, host, bus = env
    bus.phone_ready.emit("p1", True)
    bus.camera_watched.emit(True)
    assert host.starts == []


def test_starts_when_an_app_reads_the_camera_and_stops_after_it_lets_go(watching):
    plugin, host, bus = watching
    assert host.starts == []
    bus.camera_watched.emit(True)
    assert host.starts == [False]
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    assert plugin._watch_stop.isActive() and host.stops == 0  # not straight away
    plugin._watch_stop.timeout.emit()
    assert host.stops == 1


def test_an_app_coming_back_in_time_keeps_the_stream(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    bus.camera_watched.emit(True)
    assert not plugin._watch_stop.isActive()
    assert host.starts == [False]


def test_waits_for_the_phone_when_an_app_reads_first(env):
    plugin, host, bus = env
    plugin.set_watch_stream(True)
    bus.camera_watched.emit(True)
    assert host.starts == []
    bus.phone_ready.emit("p1", True)
    bus.phone_ready.emit("p1", True)  # every idle check; a failed start isn't retried
    assert host.starts == [False]


def test_never_stops_a_stream_it_did_not_start(watching):
    plugin, host, bus = watching
    _stream_runs(host, bus)   # the user pressed Start
    bus.camera_watched.emit(True)
    bus.camera_watched.emit(False)
    assert not plugin._watch_stop.isActive()
    assert host.starts == [] and host.stops == 0


def test_a_start_already_waking_is_not_claimed(watching):
    plugin, host, bus = watching
    host.starting = True      # the user pressed Start and the phone is waking
    bus.camera_watched.emit(True)
    host.starting = False
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    assert host.starts == [] and not plugin._watch_stop.isActive()


def test_stop_sticks_until_the_app_lets_go(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    _stream_ends(host, bus)   # the user pressed Stop mid-call
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False]
    bus.camera_watched.emit(False)
    bus.camera_watched.emit(True)  # a new call
    assert host.starts == [False, False]


def test_an_app_leaving_while_the_phone_wakes_still_stops_it(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    bus.camera_watched.emit(False)  # left before the stream got going
    assert plugin._watch_stop.isActive()
    plugin._watch_stop.timeout.emit()
    assert host.stops == 1


def test_switching_it_off_hands_the_stream_to_the_user(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    plugin.set_watch_stream(False)
    bus.camera_watched.emit(False)
    assert not plugin._watch_stop.isActive() and host.stops == 0


# ── platform/autostart.py ─────────────────────────────────────────────────────

def test_linux_desktop_entry_round_trip(monkeypatch, tmp_path):
    monkeypatch.setattr(autostart, "IS_WINDOWS", False)
    command = ["/home/luna/My Apps/Telescope/start.sh", "--minimized"]
    assert autostart.is_enabled(tmp_path) is False
    assert autostart.enable(command, tmp_path) == (True, "")
    text = (tmp_path / "autostart" / "telescope.desktop").read_text()
    assert 'Exec="/home/luna/My Apps/Telescope/start.sh" --minimized\n' in text
    assert autostart.is_enabled(tmp_path) is True
    assert autostart.disable(tmp_path) == (True, "")
    assert autostart.is_enabled(tmp_path) is False
    assert autostart.disable(tmp_path) == (True, "")  # already off


def test_exec_quoting_follows_the_desktop_entry_rules():
    assert autostart._exec_arg("/usr/bin/python3") == "/usr/bin/python3"
    assert autostart._exec_arg('/a b/"x"$y') == '"/a b/\\"x\\"\\$y"'
    assert "100%%" in autostart.desktop_entry(["/x/100%", "--minimized"])


def test_launch_command_prefers_start_sh_on_linux(monkeypatch, tmp_path):
    monkeypatch.setattr(autostart, "IS_WINDOWS", False)
    monkeypatch.delattr(sys, "frozen", raising=False)
    assert autostart.launch_command(tmp_path) == [sys.executable, str(tmp_path / "main.py"), "--minimized"]
    (tmp_path / "start.sh").write_text("#!/bin/sh\n")
    assert autostart.launch_command(tmp_path) == [str(tmp_path / "start.sh"), "--minimized"]
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    assert autostart.launch_command(tmp_path) == [str(Path(sys.executable).resolve()), "--minimized"]


def test_windows_run_key_round_trip(monkeypatch):
    monkeypatch.setattr(autostart, "IS_WINDOWS", True)
    values = {}

    class Key:
        def __enter__(self):
            return self

        def __exit__(self, *_a):
            pass

    def query(_key, name):
        if name not in values:
            raise FileNotFoundError(name)
        return values[name], 1

    def delete(_key, name):
        if name not in values:
            raise FileNotFoundError(name)
        del values[name]

    fake = types.SimpleNamespace(
        HKEY_CURRENT_USER="HKCU", KEY_SET_VALUE=2, REG_SZ=1,
        OpenKey=lambda *a: Key(), CreateKey=lambda *a: Key(),
        QueryValueEx=query, DeleteValue=delete,
        SetValueEx=lambda _k, name, _r, _t, value: values.__setitem__(name, value),
    )
    assert autostart.is_enabled(winreg=fake) is False
    autostart.enable([r"C:\Program Files\Telescope\TelescopeDesktop.exe", "--minimized"], winreg=fake)
    assert values["Telescope"] == r'"C:\Program Files\Telescope\TelescopeDesktop.exe" --minimized'
    assert autostart.is_enabled(winreg=fake) is True
    assert autostart.disable(winreg=fake) == (True, "")
    assert autostart.disable(winreg=fake) == (True, "")
    assert autostart.is_enabled(winreg=fake) is False
