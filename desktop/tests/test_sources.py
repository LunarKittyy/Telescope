"""Sources against a stand-in host: no window needed."""

import inspect
import threading
from dataclasses import replace
from types import SimpleNamespace

import pytest

import telescope.app as app_module
from telescope.phones import NOT_PAIRED, READY, Resolution, Route
from telescope.session import StreamSession
from telescope.sources import PhoneSource, SourceHost


class _Conn:
    def __init__(self):
        self.problems = []
        self.released = []

    def phone(self, _pid):
        return SimpleNamespace(name="Pixel")

    def show_problem(self, res, pid):
        self.problems.append((res.status, pid))

    def release_stream(self, pid):
        self.released.append(pid)

    def session_target(self, _pid):
        return None


class _Host:
    def __init__(self):
        self.conn = _Conn()
        self.calls = []
        self.issues = {}
        self.streaming = False
        self.focused = None
        self.waking = False

    def connection(self):
        return self.conn

    def stream_output(self):
        return None

    def show_issue(self, key, issue):
        self.issues[key] = issue

    def clear_issue(self, key):
        self.issues.pop(key, None)

    def is_streaming(self):
        return self.streaming

    def is_focused(self, source):
        return source is self.focused

    def focus_stream(self, source_id):
        self.calls.append(("focus", source_id))

    def is_waking(self):
        return self.waking

    def waking_source(self):
        return None

    def wake_progress(self, source, msg):
        self.calls.append(("progress", msg))

    def wake_done(self, source, ok, reason):
        self.calls.append(("wake_done", ok, reason))

    def apply_state(self, source, session_id, state):
        self.calls.append(("state", session_id))

    def set_session(self, source, session):
        source.session = session

    def stop_source(self, source):
        self.calls.append(("stop", source.id))
        source.session = None

    def start_source(self, source_id):
        self.calls.append(("start", source_id))

    def set_camera_on(self, on):
        self.calls.append(("camera_on", on))

    def camera_stayed_off(self, session):
        self.calls.append(("stayed_off", session.id))

    def mic_reconnected(self, source):
        self.calls.append(("mic_back", source.id))

    def lost_contact(self, source):
        self.calls.append(("lost", source.id))

    def park(self, source):
        self.calls.append(("park", source.id))

    def show_slow(self, camera_limited=False):
        self.calls.append(("slow", camera_limited))


class _Client:
    def __init__(self):
        self.sent = []

    def send(self, **params):
        self.sent.append(params)


@pytest.fixture
def phone(qapp, monkeypatch):
    monkeypatch.setattr(PhoneSource, "_spawn_state_fetch", lambda self, session_id: None)
    monkeypatch.setattr(PhoneSource, "_spawn_recovery_probe",
                        lambda self, sid, gen, job: self._on_recovery_probed(sid, gen, job()))
    host = _Host()
    source = PhoneSource(host, "phone-a")
    source.session = StreamSession(source=source, id=1, url="http://phone/v1/video", client=_Client(), worker=None)
    yield source, host
    source.end_recovery()


def _probe(source, host, res):
    host.conn.recovery_probe = lambda _pid: (lambda: res)
    source.recovering = True
    source.lost()


def test_mic_only_session_can_hand_over_to_usb(phone):
    source, host = phone
    source.camera_off = True
    route = Route("usb", "127.0.0.1", "serial")
    moved = "https://127.0.0.1:12345/v1/video.h264"
    committed = []
    host.conn.prepare_stream_route = lambda r, sid: moved
    host.conn.commit_stream_route = committed.append
    source.session.client.retarget = lambda url: setattr(source.session.client, "base", url.rsplit("/", 1)[0])
    assert source.hand_over(route)
    assert source.session.url == moved and source.session.worker is None
    assert committed == [source.id]
    assert ("mic_back", source.id) in host.calls


def test_the_window_offers_every_call_a_source_makes():
    wanted = {name for name, _ in inspect.getmembers(SourceHost, inspect.isfunction) if not name.startswith("_")}
    offered = {name for name, _ in inspect.getmembers(app_module._SourceHost, inspect.isfunction)}
    assert wanted <= offered


def test_a_phone_that_wont_take_the_stream_back_stops_it_and_says_why(phone):
    source, host = phone
    _probe(source, host, Resolution(NOT_PAIRED))
    assert ("stop", "phone-a") in host.calls and host.conn.problems == [(NOT_PAIRED, "phone-a")]


def test_a_phone_stopped_meanwhile_gets_a_banner_whose_start_starts_it_again(phone):
    source, host = phone
    _probe(source, host, Resolution(READY, Route("wifi", "192.168.1.20")))
    issue = host.issues["stopped:phone-a"]
    assert issue.title == "The phone stopped streaming"
    issue.actions[0].callback()
    assert host.calls[-1] == ("start", "phone-a")


def test_with_others_streaming_the_banner_names_the_phone(phone):
    source, host = phone
    host.streaming = True
    _probe(source, host, Resolution(READY, Route("wifi", "192.168.1.20"), streaming_for="Desk"))
    assert host.issues["stopped:phone-a"].title == "Pixel is streaming to Desk"


def test_wake_progress_only_counts_while_the_host_is_waking(phone):
    source, host = phone
    source._on_wake_progress(source._wake_id, "Waking…")
    assert host.calls == []
    host.waking = True
    source._on_wake_progress(source._wake_id, "Waking…")
    assert host.calls == [("progress", "Waking…")]


def test_a_quiet_phone_with_its_camera_off_is_looked_for(phone):
    source, host = phone
    for _ in range(2):
        source._on_alive(1, False)
    assert host.calls == [("lost", "phone-a")]


def test_a_camera_that_stays_off_says_so(phone):
    source, host = phone
    source._on_camera_check(1, {"camera_off": True, "camera_error": "In use"})
    assert source.camera_off and host.calls == [("stayed_off", 1)]
    assert host.issues["camera"].text == "In use"


def test_a_phone_that_cant_turn_its_camera_off_gets_it_back_on(phone):
    source, host = phone
    source.camera_off = True
    source.on_phone_state({"camera_toggle": False})
    assert ("camera_on", True) in host.calls and "camera" in host.issues


def test_a_stop_hands_the_route_back(phone):
    source, host = phone
    source.stopped(remote_stop=False, active=True)
    assert host.conn.released == ["phone-a"]


def test_a_wake_right_after_stop_waits_for_the_stop_to_reach_the_phone(phone):
    # Otherwise the wake joins the stream that's about to end, and the late stop ends the new session too
    source, host = phone
    calls, let_go = [], threading.Event()

    def stop_phone_streaming(target=None):
        let_go.wait(5)
        calls.append("stop")

    host.conn.stop_phone_streaming = stop_phone_streaming
    host.conn.ensure_phone_streaming = lambda **kw: calls.append(("wake", kw["after_stop"])) or (True, "")
    source._stop_phone_async()
    threading.Timer(0.2, let_go.set).start()
    source._wake_phone(source._wake_id, host.conn, "url", None)
    assert calls == ["stop", ("wake", True)]


def test_a_wake_long_after_a_stop_doesnt_wait_for_the_phone_to_settle(phone):
    source, host = phone
    calls = []
    host.conn.ensure_phone_streaming = lambda **kw: calls.append(kw["after_stop"]) or (True, "")
    source._wake_phone(source._wake_id, host.conn, "url", None)
    assert calls == [False]


class _Worker:
    def __init__(self):
        self.failed_tries = 0
        self.urls = []

    def retarget(self, url):
        self.urls.append(url)


def test_the_same_cable_back_after_a_fresh_forward_died_gets_another(phone):
    # Unplugged again after recovery made a forward but before video came back: adb dropped that forward
    source, host = phone
    usb = Route("usb", "127.0.0.1", "serial")
    source.session = replace(source.session, worker=_Worker())
    ports = iter(["https://127.0.0.1:41000/v1/video", "https://127.0.0.1:41001/v1/video"])
    host.conn.adopt_stream_route = lambda route, pid: next(ports)
    source.session.client.retarget = lambda url: None
    _probe(source, host, Resolution(READY, usb, streaming=True))
    assert source.session.worker.urls == ["https://127.0.0.1:41000/v1/video"]
    source._probe_recovery()  # still connecting on it: left alone
    assert len(source.session.worker.urls) == 1
    source.session.worker.failed_tries += 1  # the cable went, and came back
    source._probe_recovery()
    assert source.session.worker.urls[-1] == "https://127.0.0.1:41001/v1/video"
    source._probe_recovery()
    assert len(source.session.worker.urls) == 2


def test_a_wifi_route_that_hasnt_connected_yet_isnt_moved_again(phone):
    source, host = phone
    wifi = Route("wifi", "192.168.1.20")
    source.session = replace(source.session, worker=_Worker())
    adopted = []
    host.conn.adopt_stream_route = lambda route, pid: adopted.append(route) or "https://192.168.1.20:8080/v1/video"
    source.session.client.retarget = lambda url: None
    _probe(source, host, Resolution(READY, wifi, streaming=True))
    source.session.worker.failed_tries += 1
    source._probe_recovery()
    assert adopted == [wifi]
