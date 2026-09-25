"""Connection plugin: phone list, route display, stream setup, and per-phone settings."""

from types import SimpleNamespace

import pytest
from PyQt6.QtWidgets import QMessageBox

import telescope.plugins.connection as connection_module
from telescope.pairing import PairingResult
from telescope.phones import (
    DESKTOP_OUTDATED, LOCAL_ONLY, PHONE_OUTDATED, NOT_PAIRED, READY, ROUTE_AUTO, ROUTE_USB, ROUTE_WIFI, UNREACHABLE,
    USB_APP_CLOSED, USB_NEEDS_ATTENTION, USB_NO_CABLE, Resolution, Route,
)
from telescope.plugin import EventBus
from telescope.plugins.connection import (
    ConnectionPlugin, SessionTarget, problem_text, route_text, status_line,
)
from telescope.session_client import PingResult, SessionResult

WIFI = Route("wifi", "10.0.0.5")
USB = Route("usb", "127.0.0.1", "serial-1")


class _Host:
    def __init__(self):
        self.saves = 0
        self.switches = []
        self.reconnects = 0
        self.stops = 0
        self.forgotten = []
        self.streaming = False

    def schedule_save(self):
        self.saves += 1

    def save_now(self):
        self.saves += 1

    def switch_device(self, previous, new):
        self.switches.append((previous, new))

    def forget_device_settings(self, name):
        self.forgotten.append(name)

    def reconnect_stream(self):
        self.reconnects += 1

    def is_streaming(self):
        return self.streaming

    def stop_stream(self):
        if self.streaming:
            self.stops += 1
            self.streaming = False


class _FakeDiscovery:
    def start(self):
        pass

    def stop(self):
        pass

    def lookup(self, _pid):
        return []


class _FakeResolver:
    def __init__(self, result=Resolution(UNREACHABLE)):
        self.result = result
        self.calls = []

    def resolve(self, phone, preference=ROUTE_AUTO):
        self.calls.append((phone.id, preference))
        return self.result


class _FakeTunnels:
    def __init__(self, local=41000):
        self.local = local
        self.held = {}
        self.acquires = 0

    def acquire(self, serial, port):
        self.acquires += 1
        if self.local is None:
            return None
        self.held[(serial, port)] = self.held.get((serial, port), 0) + 1
        return self.local

    def release(self, serial, port):
        self.held[(serial, port)] -= 1
        if not self.held[(serial, port)]:
            del self.held[(serial, port)]


@pytest.fixture
def plugin_env(qapp, config_home, monkeypatch):
    # No real background resolves, revokes or mDNS: threads outliving a test abort PyQt.
    monkeypatch.setattr(ConnectionPlugin, "_spawn_resolve", lambda self, *a: None)
    monkeypatch.setattr(ConnectionPlugin, "_spawn_revoke", lambda self, phone: self._revoked.append(phone.id))
    monkeypatch.setattr(connection_module, "LanDiscovery", _FakeDiscovery)
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    monkeypatch.setattr(connection_module, "run_off_ui_thread", lambda fn, *a, **k: fn(*a, **k))
    host = _Host()
    plugin = ConnectionPlugin()
    plugin._revoked = []
    plugin.setup(host, EventBus())
    plugin._resolver = _FakeResolver()
    plugin._tunnels = _FakeTunnels()
    panel = plugin.create_panel()
    plugin._status_timer.stop()
    return plugin, host, panel


def _add(plugin, pid="id-a", name="Pixel", token="tok-a", ips=("10.0.0.5",)):
    plugin._on_phone_paired(PairingResult(name=name, ips=list(ips), token=token,
                                          source_ip=ips[0] if ips else None, phone_id=pid))


# ── Text ──────────────────────────────────────────────────────────────────────

def test_status_line_covers_every_state():
    assert status_line(None) == ("status_dim", "Checking…")
    assert status_line(Resolution(READY, WIFI))[0] == "status_ok"
    for status in (UNREACHABLE, LOCAL_ONLY, USB_NEEDS_ATTENTION, PHONE_OUTDATED, DESKTOP_OUTDATED):
        assert status_line(Resolution(status))[0] == "status_warn"
    assert status_line(Resolution(NOT_PAIRED))[0] == "status_err"


def test_route_text_names_the_live_route():
    assert route_text(USB) == "USB cable"
    assert route_text(WIFI) == "Wi-Fi · 10.0.0.5"
    assert route_text(None) == "—"


def test_problem_text_says_what_to_do():
    assert "Open Telescope on Pixel" in problem_text(Resolution(UNREACHABLE), "Pixel", ROUTE_AUTO)
    assert "Add phone" in problem_text(Resolution(NOT_PAIRED), "Pixel", ROUTE_AUTO)
    assert "Local only" in problem_text(Resolution(LOCAL_ONLY), "Pixel", ROUTE_AUTO)
    forced = problem_text(Resolution(USB_NEEDS_ATTENTION, usb_note=USB_APP_CLOSED), "Pixel", ROUTE_USB)
    assert "isn't open" in forced and "Automatic" in forced
    assert "Update it on the phone" in problem_text(Resolution(PHONE_OUTDATED), "Pixel", ROUTE_AUTO)
    assert "on this computer" in problem_text(Resolution(DESKTOP_OUTDATED), "Pixel", ROUTE_AUTO)
    assert problem_text(Resolution(READY, WIFI), "Pixel", ROUTE_AUTO) == ""


# ── Config ────────────────────────────────────────────────────────────────────

def test_config_round_trips_and_keeps_the_computer_identity(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin.set_route_preference(ROUTE_WIFI)
    cfg = plugin.get_config()

    other = ConnectionPlugin()
    other.setup(_Host(), EventBus())
    other._resolver = _FakeResolver()
    _other_panel = other.create_panel()
    other._status_timer.stop()
    other.set_config(cfg)

    assert other.get_config() == cfg
    assert cfg["computer_id"] and cfg["route"] == ROUTE_WIFI
    assert cfg["phones"][0]["id"] == "id-a"


def test_set_config_discards_malformed_phones_and_bad_values(plugin_env):
    plugin, _host, _panel = plugin_env
    plugin.set_config({
        "route": "carrier-pigeon",
        "phones": [{"id": "id-a", "name": "A", "token": "t"}, {"name": "no id"}, "junk"],
        "selected_phone": "gone",
        "computer_name": "   ",
    })
    assert [p.id for p in plugin.phones] == ["id-a"]
    assert plugin.selected_device == "id-a"
    assert plugin.get_config()["route"] == ROUTE_AUTO
    assert plugin.get_config()["computer_name"]


def test_empty_state_is_just_the_status(plugin_env):
    plugin, _host, _panel = plugin_env
    assert plugin._status_lbl.text() == "No phone yet"
    assert plugin._note_row.isHidden()  # the Add phone button in the card header says the rest
    assert plugin._route_row.isHidden()


# ── Pairing and the phone list ────────────────────────────────────────────────

def test_pairing_adds_and_selects_the_phone(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin)
    assert plugin.selected_device == "id-a"
    assert plugin.phone("id-a").active_ip == "10.0.0.5"
    assert host.switches == [(None, "id-a")]
    assert plugin._phone_combo.currentText() == "Pixel"


def test_pairing_again_replaces_the_token_instead_of_adding_a_duplicate(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin, token="old")
    _add(plugin, token="new", ips=("10.0.0.9",))
    assert len(plugin.phones) == 1
    assert plugin.phone("id-a").token == "new"
    assert plugin.phone("id-a").ips == ["10.0.0.9"]


def test_selecting_another_phone_switches_its_settings_profile(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin, pid="id-a", name="A")
    _add(plugin, pid="id-b", name="B")
    host.switches.clear()
    plugin._phone_combo.setCurrentIndex(0)
    assert plugin.selected_device == "id-a"
    assert host.switches == [("id-b", "id-a")]


def test_switching_phones_mid_stream_stops_the_stream(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin, pid="id-a", name="A")
    _add(plugin, pid="id-b", name="B")
    plugin._streaming = True
    host.streaming = True
    plugin._select("id-a")
    assert host.stops == 1


def test_forgetting_a_phone_revokes_it_and_drops_its_settings(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin, pid="id-a", name="A")
    _add(plugin, pid="id-b", name="B")
    plugin.forget_phone("id-b")
    assert [p.id for p in plugin.phones] == ["id-a"]
    assert plugin.selected_device == "id-a"
    assert host.forgotten == ["id-b"]
    assert plugin._revoked == ["id-b"]

    plugin.forget_phone("id-a")
    assert plugin.selected_device is None
    assert plugin._status_lbl.text() == "No phone yet"


def test_rename_updates_the_picker(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin.rename_phone("id-a", "Desk cam")
    assert plugin._phone_combo.currentText() == "Desk cam"


def test_phones_dialog_lists_renames_and_removes(plugin_env, monkeypatch):
    plugin, host, _panel = plugin_env
    _add(plugin, pid="id-a", name="A")
    dialog = connection_module.PhonesDialog(plugin)
    assert dialog._list.count() == 1
    dialog._list.setCurrentRow(0)
    monkeypatch.setattr(connection_module.QInputDialog, "getText", lambda *a, **k: ("Renamed", True))
    dialog._rename()
    assert dialog._list.item(0).text() == "Renamed"
    monkeypatch.setattr(connection_module.QMessageBox, "question",
                        lambda *a: QMessageBox.StandardButton.Yes)
    dialog._list.setCurrentRow(0)
    dialog._remove()
    assert dialog._list.count() == 0
    assert host.forgotten == ["id-a"]


# ── Status ────────────────────────────────────────────────────────────────────

def test_resolution_updates_the_card_and_remembers_the_address_that_answered(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin, ips=("10.0.0.5",))
    saves = host.saves
    plugin._check_id = 7
    plugin._on_resolved(7, "id-a", Resolution(READY, Route("wifi", "10.0.0.77"), usb_note=USB_NO_CABLE))
    assert plugin._status_lbl.text() == "● Ready"
    assert plugin._using_lbl.text() == "Wi-Fi · 10.0.0.77"
    assert plugin._note_row.isHidden()  # no cable, nothing to explain
    assert plugin.phone("id-a").active_ip == "10.0.0.77"
    assert "10.0.0.77" in plugin.phone("id-a").ips
    assert host.saves > saves


def test_a_cable_that_is_not_used_gets_explained(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin._apply_resolution(Resolution(READY, WIFI, usb_note=USB_APP_CLOSED))
    assert plugin._note_lbl.text() == "Not using USB: phone plugged in, but Telescope isn't open on it."


class _InlineThread:
    def __init__(self, target, args=(), kwargs=None, daemon=None):
        self._run = lambda: target(*args, **(kwargs or {}))

    def start(self):
        self._run()


def test_an_outdated_phone_on_usb_can_be_updated_from_here(plugin_env, monkeypatch, tmp_path):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    apk = tmp_path / "Telescope.apk"
    apk.write_bytes(b"apk")
    monkeypatch.setattr(connection_module, "bundled_apk_path", lambda: apk)
    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "threading", SimpleNamespace(Thread=_InlineThread))
    installs = []
    monkeypatch.setattr(connection_module, "adb_install",
                        lambda serial, path: installs.append((serial, path)) or (True, ""))

    plugin._apply_resolution(Resolution(PHONE_OUTDATED, WIFI))
    assert plugin._update_row.isHidden()  # over Wi-Fi the phone updates itself
    plugin._apply_resolution(Resolution(PHONE_OUTDATED, USB))
    assert not plugin._update_row.isHidden() and plugin._update_btn.text() == "Update over USB"
    plugin._update_btn.click()
    assert installs == [("serial-1", apk)]
    assert plugin._note_lbl.text() == "Phone app updated. Open Telescope on the phone."
    plugin._apply_resolution(Resolution(READY, USB))
    assert plugin._update_row.isHidden() and plugin._note_row.isHidden()


def test_a_failed_phone_update_says_why(plugin_env, monkeypatch, tmp_path):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    monkeypatch.setattr(connection_module, "bundled_apk_path", lambda: tmp_path / "Telescope.apk")
    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "threading", SimpleNamespace(Thread=_InlineThread))
    monkeypatch.setattr(connection_module, "adb_install", lambda *_a: (False, "signed differently"))
    plugin._apply_resolution(Resolution(PHONE_OUTDATED, USB))
    plugin._update_btn.click()
    assert plugin._note_lbl.text() == "signed differently"
    assert plugin._note_lbl.objectName() == "status_err"


def test_a_newer_phone_app_points_at_this_app_s_update(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    asked = []
    plugin._bus.update_requested.connect(lambda: asked.append(True))
    plugin._apply_resolution(Resolution(DESKTOP_OUTDATED, WIFI))
    assert plugin._update_btn.text() == "Update this app"
    plugin._update_btn.click()
    assert asked == [True]


def test_stale_resolutions_are_ignored(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin._check_id = 3
    plugin._on_resolved(2, "id-a", Resolution(READY, WIFI))
    plugin._on_resolved(3, "someone-else", Resolution(READY, WIFI))
    assert plugin.resolution is None


def test_a_problem_shows_its_fix_on_the_card(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin._apply_resolution(Resolution(NOT_PAIRED))
    assert "Needs pairing again" in plugin._status_lbl.text()
    assert "Add phone" in plugin._note_lbl.text()


def test_changing_the_route_saves_and_reconnects_a_live_stream(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin)
    plugin._streaming = True
    plugin._route_combo.setCurrentIndex(plugin._route_combo.findData(ROUTE_USB))
    assert plugin.get_config()["route"] == ROUTE_USB
    assert host.reconnects == 1
    plugin.set_route_preference(ROUTE_WIFI)
    assert plugin._route_combo.currentText() == "Wi-Fi only"


# ── Starting a stream ─────────────────────────────────────────────────────────

def test_start_without_a_phone_explains_and_refuses(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    shown = []
    monkeypatch.setattr(connection_module.QMessageBox, "information", lambda *a: shown.append(a[1]))
    assert plugin.get_stream_info() == (None, None, False)
    assert shown == ["No phone yet"]


def test_start_on_an_unreachable_phone_shows_the_fix(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    shown = []
    monkeypatch.setattr(connection_module.QMessageBox, "warning", lambda *a: shown.append(a[2]))
    assert plugin.get_stream_info() == (None, None, False)
    assert "Open Telescope on Pixel" in shown[0]


def test_start_over_wifi_uses_the_resolved_address(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin, token="tok")
    plugin._resolver.result = Resolution(READY, WIFI)
    assert plugin.get_stream_info() == ("http://10.0.0.5:8080/v1/video", "tok", True)
    assert plugin.session_target() == SessionTarget("tok", WIFI)


def test_start_over_usb_holds_a_forward_until_the_stream_stops(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin, token="tok")
    plugin._resolver.result = Resolution(READY, USB)
    assert plugin.get_stream_info() == ("http://127.0.0.1:41000/v1/video", "tok", True)
    assert plugin._tunnels.held == {("serial-1", 8080): 1}
    plugin.on_stream_start("url", None)
    plugin.on_stream_stop()
    assert plugin._tunnels.held == {}


def test_start_over_usb_reports_a_failed_forward(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin._resolver.result = Resolution(READY, USB)
    plugin._tunnels.local = None
    shown = []
    monkeypatch.setattr(connection_module.QMessageBox, "warning", lambda *a: shown.append(a[2]))
    assert plugin.get_stream_info() == (None, None, False)
    assert "adb" in shown[0]


def test_linux_virtual_camera_conflict_and_cancel(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    monkeypatch.setattr(connection_module, "IS_LINUX", True)
    monkeypatch.setattr(connection_module, "v4l2_devices_ready", lambda: False)
    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: True)
    warnings = []
    monkeypatch.setattr(connection_module.QMessageBox, "warning", lambda *a: warnings.append(a[1]))
    assert plugin.get_stream_info() == (None, None, False)
    assert warnings == ["Virtual camera is set up differently"]

    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(connection_module.QMessageBox, "question",
                        lambda *a: QMessageBox.StandardButton.Cancel)
    assert plugin.get_stream_info() == (None, None, False)


def test_linux_virtual_camera_load_failure_and_success(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    monkeypatch.setattr(connection_module, "IS_LINUX", True)
    monkeypatch.setattr(connection_module, "v4l2_devices_ready", lambda: False)
    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(connection_module.QMessageBox, "question",
                        lambda *a: QMessageBox.StandardButton.Ok)
    errors = []
    monkeypatch.setattr(connection_module.QMessageBox, "critical", lambda *a: errors.append(a[1]))
    monkeypatch.setattr(connection_module, "v4l2_load", lambda: (False, "denied"))
    assert plugin.get_stream_info() == (None, None, False)
    assert errors == ["Couldn't set up the virtual camera"]

    _add(plugin)
    plugin._resolver.result = Resolution(READY, WIFI)
    monkeypatch.setattr(connection_module, "v4l2_load", lambda: (True, "ok"))
    assert plugin.get_stream_info()[2] is True


# ── Plugged in mid-stream ─────────────────────────────────────────────────────

def test_streaming_over_wifi_in_automatic_watches_for_usb(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin)
    plugin._stream_route = WIFI
    plugin.on_stream_start("url", None)
    assert plugin._usb_watch_timer.isActive()

    plugin._watch_id = 4
    plugin._on_usb_available(4, True)
    assert not plugin._switch_usb_row.isHidden()
    plugin._switch_usb_btn.click()
    assert host.reconnects == 1
    assert plugin._switch_usb_row.isHidden()


def test_no_usb_watch_when_wifi_was_chosen_explicitly_or_already_on_usb(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin._route_pref = ROUTE_WIFI
    plugin._stream_route = WIFI
    plugin.on_stream_start("url", None)
    assert not plugin._usb_watch_timer.isActive()
    plugin.on_stream_stop()

    plugin._route_pref = ROUTE_AUTO
    plugin._stream_route = USB
    plugin.on_stream_start("url", None)
    assert not plugin._usb_watch_timer.isActive()


def test_a_usb_answer_after_the_stream_stopped_is_ignored(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin._stream_route = WIFI
    plugin.on_stream_start("url", None)
    plugin._watch_id = 4
    plugin.on_stream_stop()
    plugin._on_usb_available(4, True)
    assert plugin._switch_usb_row.isHidden()


# ── Session channel and waking the phone ──────────────────────────────────────

def test_session_channel_over_usb_releases_its_forward_even_on_error(plugin_env):
    plugin, _host, _panel = plugin_env
    with pytest.raises(RuntimeError):
        with plugin.session_channel(SessionTarget("tok", USB)) as client:
            assert client.base == "http://127.0.0.1:41000"
            raise RuntimeError
    assert plugin._tunnels.held == {}


def test_session_channel_without_a_route_or_token_yields_none(plugin_env):
    plugin, _host, _panel = plugin_env
    with plugin.session_channel(SessionTarget("tok", None)) as client:
        assert client is None
    with plugin.session_channel(SessionTarget(None, WIFI)) as client:
        assert client is None


def _stub_session(monkeypatch, pings, start=None):
    calls = []
    remaining = list(pings)

    def ping(self):
        calls.append("ping")
        return remaining.pop(0) if len(remaining) > 1 else remaining[0]

    monkeypatch.setattr(connection_module.PhoneSessionClient, "ping", ping)
    monkeypatch.setattr(connection_module.PhoneSessionClient, "start",
                        lambda self: calls.append("start") or (start or SessionResult(ok=True)))
    monkeypatch.setattr(connection_module.PhoneSessionClient, "stop",
                        lambda self: calls.append("stop") or SessionResult(ok=True))
    monkeypatch.setattr(connection_module.time, "sleep", lambda _s: None)
    return calls


_TARGET = SessionTarget("tok", WIFI)


def _ping(streaming=False, busy=False, status="paired"):
    return PingResult(status, streaming=streaming, busy=busy, local_only=False)


def test_ensure_streaming_is_a_no_op_when_already_streaming(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    calls = _stub_session(monkeypatch, [_ping(streaming=True)])
    assert plugin.ensure_phone_streaming(target=_TARGET) == (True, "")
    assert calls == ["ping"]


def test_ensure_streaming_starts_then_waits_for_the_camera(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    calls = _stub_session(monkeypatch, [_ping(), _ping(busy=True), _ping(streaming=True)])
    progress = []
    assert plugin.ensure_phone_streaming(on_progress=progress.append, target=_TARGET) == (True, "")
    assert calls == ["ping", "start", "ping", "ping"]
    assert progress[0] == "Starting the phone's camera..."


def test_ensure_streaming_reports_a_refused_start(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    _stub_session(monkeypatch, [_ping()], start=SessionResult(ok=False, error="no_camera_permission"))
    ok, reason = plugin.ensure_phone_streaming(target=_TARGET)
    assert not ok and "camera access" in reason


def test_ensure_streaming_gives_up_when_the_camera_falls_back_to_idle(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    _stub_session(monkeypatch, [_ping(), _ping(busy=True), _ping()])
    ok, reason = plugin.ensure_phone_streaming(target=_TARGET)
    assert not ok and "stopped before it finished" in reason


def test_ensure_streaming_tolerates_a_blip_but_not_a_sustained_outage(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    gone = PingResult("unreachable")
    _stub_session(monkeypatch, [_ping(), gone, _ping(busy=True), _ping(streaming=True)])
    assert plugin.ensure_phone_streaming(target=_TARGET) == (True, "")

    _stub_session(monkeypatch, [_ping(), gone, gone, gone])
    ok, reason = plugin.ensure_phone_streaming(target=_TARGET)
    assert not ok and "Lost contact" in reason


def test_ensure_streaming_times_out_rather_than_hanging(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    _stub_session(monkeypatch, [_ping()])
    clock = iter(range(0, 1000, 5))
    monkeypatch.setattr(connection_module.time, "monotonic", lambda: next(clock))
    ok, reason = plugin.ensure_phone_streaming(target=_TARGET)
    assert not ok and "didn't finish starting in time" in reason


def test_ensure_streaming_explains_unreachable_and_unpaired(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    _stub_session(monkeypatch, [PingResult("unreachable")])
    assert "Couldn't reach" in plugin.ensure_phone_streaming(target=_TARGET)[1]
    _stub_session(monkeypatch, [PingResult("not_paired")])
    assert "Add phone" in plugin.ensure_phone_streaming(target=_TARGET)[1]
    assert "Lost the connection" in plugin.ensure_phone_streaming(target=SessionTarget("tok", None))[1]


def test_stop_phone_streaming_posts_stop(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    calls = _stub_session(monkeypatch, [_ping()])
    plugin.stop_phone_streaming(target=_TARGET)
    assert calls == ["stop"]
    plugin.stop_phone_streaming(target=SessionTarget("tok", None))
    assert calls == ["stop"]


# ── Header ────────────────────────────────────────────────────────────────────

def test_header_holds_the_phone_picker_and_manage_button(plugin_env):
    plugin, _host, _panel = plugin_env
    header = plugin.create_header_widget()
    assert plugin._phone_combo.parent() is header
    assert plugin._manage_btn.parent() is header


# ── Per-phone settings through the real window ────────────────────────────────

@pytest.fixture
def window_with_plugins(qapp, config_home, monkeypatch):
    from telescope.app import TelescopeWindow
    from telescope.plugins.camera_control import CameraControlPlugin
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.plugins.stream_output import StreamOutputPlugin
    from telescope.plugins.transforms import TransformsPlugin

    monkeypatch.setattr(ConnectionPlugin, "_spawn_resolve", lambda self, *a: None)
    monkeypatch.setattr(ConnectionPlugin, "_spawn_revoke", lambda self, phone: None)
    monkeypatch.setattr(connection_module, "LanDiscovery", _FakeDiscovery)

    def build():
        win = TelescopeWindow()
        conn, cam = ConnectionPlugin(), CameraControlPlugin()
        for p in (conn, cam, StreamOutputPlugin(), TransformsPlugin(), MonitoringPlugin()):
            win.register_plugin(p)
        win.apply_saved_config()
        conn._status_timer.stop()
        return win, conn, cam

    return build


def test_each_phone_keeps_its_own_settings(window_with_plugins):
    win, conn, cam = window_with_plugins()
    _add(conn, pid="id-a", name="A")
    default_iso = cam.get_config()["iso"]
    cam._rb_exp_manual.setChecked(True)
    cam._iso_slider.set_value(400)
    _add(conn, pid="id-b", name="B")
    assert cam.get_config()["iso"] == pytest.approx(default_iso, abs=1)

    conn._select("id-a")
    assert cam.get_config()["iso"] == pytest.approx(400, abs=1)


def test_phones_and_selection_survive_a_restart(window_with_plugins):
    win, conn, _cam = window_with_plugins()
    _add(conn, pid="id-a", name="A", token="tok-a")
    _add(conn, pid="id-b", name="B", token="tok-b")
    conn._select("id-a")
    win.save_now()

    _win2, conn2, _cam2 = window_with_plugins()
    assert conn2.selected_device == "id-a"
    assert conn2.phone("id-b").token == "tok-b"
    assert conn2._active_key == "id-a"


def test_removing_a_phone_deletes_its_stored_settings(window_with_plugins, config_home):
    win, conn, _cam = window_with_plugins()
    _add(conn, pid="id-a", name="A")
    _add(conn, pid="id-b", name="B")
    win.save_now()
    assert "id-b" in config_home.load_config()["devices"]
    conn.forget_phone("id-b")
    assert "id-b" not in config_home.load_config()["devices"]


def test_phone_count_is_announced_on_the_bus(plugin_env):
    plugin, _host, _panel = plugin_env
    counts = []
    plugin._bus.phones_changed.connect(counts.append)
    _add(plugin)
    plugin.forget_phone("id-a")
    assert counts[-1] == 0 and 1 in counts


def test_add_phone_request_on_the_bus_opens_pairing(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    opened = []

    class _FakeDialog:
        def __init__(self, parent, computer_id, computer_name, on_paired):
            opened.append(computer_name)

        def setWindowModality(self, _m):
            pass

        def show(self):
            pass

    monkeypatch.setattr(connection_module, "AddPhoneDialog", _FakeDialog)
    plugin._bus.add_phone_requested.emit()
    assert opened == [plugin.computer_name]


def test_computer_name_can_be_renamed_from_the_phones_dialog(plugin_env, monkeypatch):
    plugin, _host, _panel = plugin_env
    dialog = connection_module.PhonesDialog(plugin)
    monkeypatch.setattr(connection_module.QInputDialog, "getText", lambda *a, **k: ("Studio PC", True))
    dialog._rename_computer()
    assert plugin.get_config()["computer_name"] == "Studio PC"
    assert dialog._computer_lbl.text() == "Studio PC"


def test_re_pairing_the_streaming_phone_reconnects_with_the_new_token(plugin_env):
    plugin, host, _panel = plugin_env
    _add(plugin, token="old")
    plugin._streaming = True
    _add(plugin, token="new")
    assert host.reconnects == 1
    _add(plugin, pid="id-b", name="Other")  # pairing a different phone doesn't touch the stream
    assert host.reconnects == 1


def test_card_says_connecting_until_the_first_frame(plugin_env):
    plugin, _host, _panel = plugin_env
    _add(plugin)
    plugin._stream_route = WIFI
    plugin.on_stream_start("url", None)
    assert plugin._status_lbl.text() == "Connecting…"
    plugin._bus.stream_connected.emit()
    assert plugin._status_lbl.text() == "● Streaming"
