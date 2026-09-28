"""Streaming by itself when the phone is ready, and opening at sign-in."""

import sys
import types
from pathlib import Path

import pytest
from PyQt6.QtWidgets import QMenu, QWidgetAction

import telescope.platform.autostart as autostart
import telescope.plugins.startup as startup_module
from telescope.plugin import EventBus
from telescope.plugins.startup import StartupPlugin, StopDelayDialog


class _Host:
    def __init__(self):
        self.streaming = False
        self.starting = False
        self.restarting = False
        self.starts = []
        self.stops = 0
        self.keep = None
        self.saves = 0
        self.issues = {}
        self.notes = []

    def is_streaming(self):
        return self.streaming

    def is_starting(self):
        return self.starting

    def is_restarting(self):
        return self.restarting

    def start_stream(self, interactive=True):
        self.starts.append(interactive)
        self.starting = True  # like the real host: waking the phone until the stream starts or fails

    def stop_stream(self):
        self.stops += 1
        self.starting = False

    def set_keep_in_tray(self, keep):
        self.keep = keep

    def schedule_save(self):
        self.saves += 1

    def show_issue(self, key, issue):
        self.issues[key] = issue

    def clear_issue(self, key=None):
        self.issues.pop(key, None)

    def send_notification(self, title, body, urgent=True):
        self.notes.append((title, body, urgent))


def _start_fails(host, bus):
    host.starting = False
    bus.stream_start_failed.emit()


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
    plugin.set_start_on("ready")
    assert host.keep is True
    bus.phone_ready.emit("p1", True)
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False]
    _start_fails(host, bus)
    bus.phone_ready.emit("p1", True)    # a failed start isn't retried on every status check
    assert host.starts == [False]
    bus.phone_ready.emit("p1", False)   # phone went away
    bus.phone_ready.emit("p1", True)    # and came back
    assert host.starts == [False, False]


def test_stop_sticks_until_the_phone_goes_away(env):
    plugin, host, bus = env
    plugin.set_start_on("ready")
    bus.stream_stopped.emit()
    bus.phone_ready.emit("p1", True)
    assert host.starts == []
    bus.phone_ready.emit("p1", False)
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False]


def test_switching_phones_is_a_new_arrival(env):
    plugin, host, bus = env
    plugin.set_start_on("ready")
    bus.phone_ready.emit("p1", True)
    _start_fails(host, bus)
    bus.phone_ready.emit("p2", True)
    assert host.starts == [False, False]


def test_not_while_a_start_is_waking_the_phone(env):
    plugin, host, bus = env
    plugin.set_start_on("ready")
    host.starting = True  # the user pressed Start; claiming it would make their stream stop by itself
    bus.phone_ready.emit("p1", True)
    assert host.starts == [] and plugin._starting_own is False


def test_not_while_streaming(env):
    plugin, host, bus = env
    plugin.set_start_on("ready")
    host.streaming = True
    bus.phone_ready.emit("p1", True)
    assert host.starts == []


@pytest.mark.parametrize("old, start_on, stop_idle", [
    ({}, "off", "off"),
    ({"auto_stream": True}, "ready", "off"),
    ({"watch_stream": True}, "watched", "own"),
    ({"auto_stream": True, "watch_stream": True}, "watched", "own"),  # both on from before: watching wins
    ({"auto_stream": "yes", "watch_stream": 1}, "off", "off"),
])
def test_the_two_old_checkboxes_carry_over(env, old, start_on, stop_idle):
    plugin, host, _bus = env
    plugin.set_config(old)
    assert (plugin.start_on, plugin.stop_idle, plugin.stop_delay_s, plugin.notify_stop) \
        == (start_on, stop_idle, 15, True)
    assert host.keep is (start_on != "off")


def test_config_round_trip_and_bad_values(env):
    plugin, host, _bus = env
    cfg = {"start_on": "watched", "stop_idle": "any", "stop_delay_s": 300, "notify_stop": False}
    plugin.set_config(cfg)
    assert plugin.get_config() == cfg and host.keep is True
    plugin.set_config({"start_on": "sometimes", "stop_idle": None, "stop_delay_s": "5", "notify_stop": "no"})
    assert plugin.get_config() == {"start_on": "off", "stop_idle": "off", "stop_delay_s": 15, "notify_stop": True}
    assert host.keep is False
    plugin.set_config({"stop_idle": "own", "stop_delay_s": 2, "auto_stream": True})  # new keys win over old ones
    assert (plugin.start_on, plugin.stop_idle, plugin.stop_delay_s) == ("off", "own", 10)
    plugin.set_config({"start_on": "ready", "stop_delay_s": 99_999})
    assert plugin.stop_delay_s == 3600
    plugin.set_config({"start_on": "ready", "stop_delay_s": True})
    assert plugin.stop_delay_s == 15


def test_diagnostics_say_what_is_on_and_who_owns_the_stream(env):
    plugin, _host, bus = env
    plugin.set_start_on("watched")
    plugin.set_stop_idle("own")
    bus.phone_ready.emit("p1", True)
    bus.camera_watched.emit(True)
    bus.stream_started.emit("u")
    assert plugin.diagnostics() == {
        "Start streaming": "When an app opens the camera",
        "Stop streaming": "When no app uses the camera, if it started the stream",
        "Wait before stopping": "15 s",
        "Tell me when it stops a stream": "on",
        "Stream started automatically": "yes",
    }


def _submenu_actions(menu):
    return {a.text(): a for a in menu.actions() if a.text() and not a.isSeparator()}


def test_menu_is_one_submenu_and_the_sign_in_entry(env, monkeypatch):
    plugin, host, _bus = env
    calls = []
    monkeypatch.setattr(startup_module.autostart, "is_enabled", lambda: False)
    monkeypatch.setattr(startup_module.autostart, "enable", lambda: calls.append("on") or (True, ""))
    monkeypatch.setattr(startup_module.autostart, "disable", lambda: calls.append("off") or (False, "denied"))
    divider, menu, sign_in = plugin.create_menu_actions()
    assert divider.isSeparator() and isinstance(menu, QMenu) and menu.title() == "Automatic streaming"
    assert not sign_in.isChecked()
    sign_in.setChecked(True)
    sign_in.setChecked(False)
    assert calls == ["on", "off"]
    assert host.issues["startup"].title == "denied"


def test_submenu_radios_reflect_and_change_the_settings(env):
    plugin, host, _bus = env
    _divider, menu, _sign_in = plugin.create_menu_actions()
    items = _submenu_actions(menu)
    headings = [a.defaultWidget().text() for a in menu.actions() if isinstance(a, QWidgetAction)]
    assert headings == ["START", "STOP"]
    assert items["Only when I press Start"].isChecked() and items["Only when I press Stop"].isChecked()
    delay = items["Wait before stopping: 15 s…"]
    notify = items["Tell me when it stops a stream"]
    assert not delay.isEnabled() and not notify.isEnabled()  # nothing stops by itself yet

    items["When an app opens the camera"].trigger()
    assert plugin.start_on == "watched" and host.keep is True
    assert not items["Only when I press Start"].isChecked()  # one start choice at a time
    items["When the phone is ready"].trigger()
    assert plugin.start_on == "ready" and not items["When an app opens the camera"].isChecked()
    items["When no app uses the camera, even if I started it"].trigger()
    assert plugin.stop_idle == "any"

    _d, menu, _s = plugin.create_menu_actions()  # rebuilt on every open
    items = _submenu_actions(menu)
    assert items["When the phone is ready"].isChecked()
    assert items["When no app uses the camera, even if I started it"].isChecked()
    assert items["Wait before stopping: 15 s…"].isEnabled()
    items["Tell me when it stops a stream"].setChecked(False)
    assert plugin.notify_stop is False


def test_delay_dialog_reads_and_writes_the_wait(env):
    plugin, host, _bus = env
    plugin.set_config({"stop_idle": "any", "stop_delay_s": 120})
    dlg = StopDelayDialog(plugin)
    dlg.refresh()
    assert dlg._unit.currentText() == "minutes" and dlg._value.value() == 2
    dlg._unit.setCurrentIndex(0)                 # to seconds: the same wait
    assert dlg._value.value() == 120 and dlg.seconds() == 120
    dlg._value.setValue(90)
    dlg._unit.setCurrentIndex(1)                 # 90 s rounds to 2 min
    assert dlg._value.value() == 2
    dlg._unit.setCurrentIndex(0)
    assert dlg._value.minimum() == 10 and dlg._value.maximum() == 3600
    dlg._value.setValue(45)
    dlg._save()
    assert plugin.stop_delay_s == 45 and host.saves > 0
    dlg.refresh()
    assert dlg._unit.currentText() == "seconds" and dlg._value.value() == 45
    dlg._unit.setCurrentIndex(1)
    assert dlg._value.minimum() == 1 and dlg._value.maximum() == 60


# ── Streaming while an app uses the camera ─────────────────────────────────────

@pytest.fixture
def watching(env):
    plugin, host, bus = env
    plugin.set_start_on("watched")
    plugin.set_stop_idle("own")
    bus.phone_ready.emit("p1", True)
    return plugin, host, bus


def _stream_runs(host, bus):
    host.starting = False
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
    assert plugin._idle_stop.isActive() and host.stops == 0  # not straight away
    plugin._idle_stop.timeout.emit()
    assert host.stops == 1


def test_an_app_coming_back_in_time_keeps_the_stream(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    bus.camera_watched.emit(True)
    assert not plugin._idle_stop.isActive()
    assert host.starts == [False]


def test_waits_for_the_phone_when_an_app_reads_first(env):
    plugin, host, bus = env
    plugin.set_start_on("watched")
    plugin.set_stop_idle("own")
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
    assert not plugin._idle_stop.isActive()
    assert host.starts == [] and host.stops == 0


def test_a_start_already_waking_is_not_claimed(watching):
    plugin, host, bus = watching
    host.starting = True      # the user pressed Start and the phone is waking
    bus.camera_watched.emit(True)
    host.starting = False
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    assert host.starts == [] and not plugin._idle_stop.isActive()


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


def test_a_reconnect_keeps_the_stream_its_own(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    host.restarting = True    # a reconnect or camera resize
    _stream_ends(host, bus)
    host.restarting = False
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    plugin._idle_stop.timeout.emit()
    assert host.starts == [False] and host.stops == 1


def test_an_app_leaving_while_the_phone_wakes_still_stops_it(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    bus.camera_watched.emit(False)  # left before the stream got going
    assert plugin._idle_stop.isActive()
    plugin._idle_stop.timeout.emit()
    assert host.stops == 1


def test_switching_it_off_hands_the_stream_to_the_user(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    plugin.set_start_on("off")
    bus.camera_watched.emit(False)
    assert not plugin._idle_stop.isActive() and host.stops == 0


# ── Stopping once nothing reads the camera ─────────────────────────────────────

def _fire(plugin):
    assert plugin._idle_stop.isActive()
    plugin._idle_stop.timeout.emit()


def test_a_manual_stream_stops_only_under_any(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    for mode, stops in (("off", 0), ("own", 0), ("any", 1)):
        plugin.set_stop_idle(mode)
        _stream_runs(host, bus)   # the user pressed Start
        if stops:
            _fire(plugin)
        else:
            assert not plugin._idle_stop.isActive()
        assert host.stops == stops
        _stream_ends(host, bus)
    assert host.notes == [("Telescope stopped streaming", "No app used the camera for 15 s.", False)]


def test_waits_the_chosen_time(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_config({"stop_idle": "any", "stop_delay_s": 300})
    _stream_runs(host, bus)
    assert plugin._idle_stop.interval() == 300_000


def test_changing_the_wait_mid_count_restarts_it(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_stop_idle("any")
    _stream_runs(host, bus)
    plugin.set_stop_delay(60)
    assert plugin._idle_stop.isActive() and plugin._idle_stop.interval() == 60_000
    plugin.set_stop_delay(3)  # below the floor
    assert plugin.stop_delay_s == 10 and plugin._idle_stop.interval() == 10_000


def test_no_notice_when_it_is_switched_off(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_stop_idle("any")
    plugin.set_notify_stop(False)
    _stream_runs(host, bus)
    _fire(plugin)
    assert host.stops == 1 and host.notes == []


def test_never_stops_before_the_watch_has_reported(env):
    plugin, host, bus = env
    plugin.set_stop_idle("any")
    _stream_runs(host, bus)   # the watch can't open the camera, so it never says anything
    assert not plugin._idle_stop.isActive()
    bus.camera_watched.emit(False)
    assert plugin._idle_stop.isActive()


def test_an_app_opening_the_camera_cancels_the_wait(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_stop_idle("any")
    _stream_runs(host, bus)
    bus.camera_watched.emit(True)
    assert not plugin._idle_stop.isActive()


def test_the_wait_ending_checks_again_before_stopping(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_stop_idle("any")
    _stream_runs(host, bus)
    plugin._watched = True    # an app arrived in the same moment the timer fired
    plugin._idle_stop.timeout.emit()
    assert host.stops == 0


def test_a_restart_never_stops_or_shortens(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_stop_idle("any")
    _stream_runs(host, bus)
    host.restarting = True
    _stream_ends(host, bus)
    host.restarting = False
    _stream_runs(host, bus)
    assert plugin._idle_stop.isActive() and host.stops == 0


def test_switching_the_stop_off_cancels_the_wait(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_stop_idle("any")
    _stream_runs(host, bus)
    plugin.set_stop_idle("off")
    assert not plugin._idle_stop.isActive()


def test_a_failed_start_does_not_mark_the_next_manual_stream(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    assert host.starts == [False]
    _start_fails(host, bus)   # the phone couldn't be woken
    _stream_runs(host, bus)   # then the user pressed Start by hand
    bus.camera_watched.emit(False)
    assert not plugin._idle_stop.isActive() and host.stops == 0


def test_phone_ready_start_with_idle_stop_stays_stopped(env):
    plugin, host, bus = env
    bus.camera_watched.emit(False)
    plugin.set_start_on("ready")
    plugin.set_stop_idle("own")
    bus.phone_ready.emit("p1", True)
    _stream_runs(host, bus)
    _fire(plugin)
    _stream_ends(host, bus)
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False] and host.stops == 1  # no start/stop loop
    bus.phone_ready.emit("p1", False)
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False, False]


def test_watch_start_without_idle_stop_keeps_streaming(watching):
    plugin, host, bus = watching
    plugin.set_stop_idle("off")
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    assert not plugin._idle_stop.isActive() and host.stops == 0


def test_an_idle_stop_lets_the_next_app_start_it_again(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    bus.camera_watched.emit(False)
    _fire(plugin)
    _stream_ends(host, bus)
    bus.camera_watched.emit(True)
    assert host.starts == [False, False]


def test_an_outside_stop_while_an_app_reads_holds_the_watch_start(watching):
    plugin, host, bus = watching
    bus.camera_watched.emit(True)
    _stream_runs(host, bus)
    _stream_ends(host, bus)   # Stop pressed, or Monitoring stopped it for battery or heat
    bus.phone_ready.emit("p1", True)
    assert host.starts == [False]


# ── platform/autostart.py ─────────────────────────────────────────────────────

def test_linux_desktop_entry_round_trip(monkeypatch, tmp_path):
    monkeypatch.setattr(autostart, "IS_WINDOWS", False)
    command = ["/home/someone/My Apps/Telescope/start.sh", "--minimized"]
    assert autostart.is_enabled(tmp_path) is False
    assert autostart.enable(command, tmp_path) == (True, "")
    text = (tmp_path / "autostart" / "telescope.desktop").read_text()
    assert 'Exec="/home/someone/My Apps/Telescope/start.sh" --minimized\n' in text
    assert autostart.is_enabled(tmp_path) is True
    assert autostart.disable(tmp_path) == (True, "")
    assert autostart.is_enabled(tmp_path) is False
    assert autostart.disable(tmp_path) == (True, "")  # already off


def test_the_sign_in_entry_follows_this_copy_when_it_is_on(monkeypatch, tmp_path):
    monkeypatch.setattr(autostart, "IS_WINDOWS", False)
    assert autostart.refresh(["/new/start.sh", "--minimized"], tmp_path) is False  # off stays off
    assert not autostart.is_enabled(tmp_path)
    autostart.enable(["/old/start.sh", "--minimized"], tmp_path)
    assert autostart.refresh(["/new/start.sh", "--minimized"], tmp_path) is True
    assert "Exec=/new/start.sh --minimized\n" in (tmp_path / "autostart" / "telescope.desktop").read_text()
    assert autostart.refresh(["/new/start.sh", "--minimized"], tmp_path) is False  # already current


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


def test_menu_entry_follows_this_copy_and_only_writes_on_change(tmp_path):
    icons = []
    save_icon = lambda path: icons.append(path) or path.write_bytes(b"png")  # noqa: E731
    command = ["/home/someone/My Apps/Telescope/start.sh"]
    assert autostart.update_menu_entry(save_icon, command, tmp_path) is True
    text = (tmp_path / "applications" / "telescope.desktop").read_text()
    assert 'Exec="/home/someone/My Apps/Telescope/start.sh"\n' in text
    assert "Icon=telescope\n" in text and "Categories=" in text and "Autostart" not in text
    assert icons == [tmp_path / "icons" / "hicolor" / "256x256" / "apps" / "telescope.png"]

    assert autostart.update_menu_entry(save_icon, command, tmp_path) is False  # nothing changed
    assert autostart.update_menu_entry(save_icon, ["/elsewhere/start.sh"], tmp_path) is True  # the folder moved
    assert "Exec=/elsewhere/start.sh\n" in (tmp_path / "applications" / "telescope.desktop").read_text()
    assert len(icons) == 1


def test_menu_entry_opens_the_window_and_skips_dev_checkouts(monkeypatch, tmp_path):
    monkeypatch.setattr(autostart, "IS_WINDOWS", False)
    monkeypatch.delattr(sys, "frozen", raising=False)
    (tmp_path / "start.sh").write_text("#!/bin/sh\n")
    assert autostart.launch_command(tmp_path, minimized=False) == [str(tmp_path / "start.sh")]
    app = tmp_path / "desktop"
    app.mkdir()
    assert not autostart.is_dev_checkout(app)
    (tmp_path / ".git").mkdir()
    assert autostart.is_dev_checkout(app)


def test_menu_entry_trouble_is_not_fatal(tmp_path):
    (tmp_path / "applications").write_text("a file where the folder should be")
    assert autostart.update_menu_entry(lambda p: True, ["/x/start.sh"], tmp_path) is False


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
    assert autostart.refresh([r"C:\New\TelescopeDesktop.exe"], winreg=fake) is False  # off stays off
    autostart.enable([r"C:\Old\TelescopeDesktop.exe"], winreg=fake)
    assert autostart.refresh([r"C:\New\TelescopeDesktop.exe"], winreg=fake) is True
    assert values["Telescope"] == r"C:\New\TelescopeDesktop.exe"
