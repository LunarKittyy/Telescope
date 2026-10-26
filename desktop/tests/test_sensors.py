"""The phone sensors service: who wants what, which phone streams it, and phones that can't."""

import io
import json
import queue
import threading
import time
import urllib.error

import pytest

from telescope.sensors import SensorHub, SensorReading, apply_line

ALL = ["orientation", "accelerometer", "gyroscope", "magnetometer", "gravity", "rotation", "light"]


class FakeResponse:
    def __init__(self):
        self.lines: "queue.Queue" = queue.Queue()
        self.closed = threading.Event()

    def send(self, obj):
        self.lines.put((json.dumps(obj) + "\n").encode())

    def readline(self, _limit=-1):
        while not self.closed.is_set():
            try:
                return self.lines.get(timeout=0.05)
            except queue.Empty:
                continue
        return b""

    def close(self):
        self.closed.set()


class FakeAuth:
    """Answers each /v1/sensors with a FakeResponse (or the HTTP error in refuse) and keeps them in opened."""

    def __init__(self, refuse: int = 0):
        self.refuse = refuse
        self.opened: list = []  # (url, response)
        self.lock = threading.Lock()

    def headers(self):
        return {"Authorization": "Bearer t"}

    def open(self, req, timeout=None):
        if self.refuse:
            raise urllib.error.HTTPError(req.full_url, self.refuse, "no", {}, io.BytesIO(b"{}"))
        resp = FakeResponse()
        with self.lock:
            self.opened.append((req.full_url, resp))
        return resp

    def live(self) -> list:
        with self.lock:
            return [(url, resp) for url, resp in self.opened if not resp.closed.is_set()]


def wait_for(cond, timeout=3.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if cond():
            return True
        time.sleep(0.01)
    return cond()


@pytest.fixture
def hub():
    changes: list = []
    h = SensorHub(on_change=changes.append)
    h.changes = changes
    yield h
    h.shutdown()


def test_nothing_streams_until_something_wants_it(hub):
    auth = FakeAuth()
    hub.follow("phone", "https://p/v1", auth, state={"sensors": ALL})
    time.sleep(0.1)
    assert auth.opened == []
    assert hub.status().state == "off" and hub.status().kinds == tuple(ALL)


def test_wants_are_counted_by_owner_and_the_stream_stops_when_nobody_wants_any(hub):
    auth = FakeAuth()
    hub.follow("phone", "https://p/v1", auth, state={"sensors": ALL})
    hub.want("level", ["orientation"])
    assert wait_for(lambda: len(auth.live()) == 1)
    assert auth.live()[0][0] == "https://p/v1/sensors?kinds=orientation"
    assert hub.status().state == "starting"
    hub.want("meter", ["light", "orientation"])
    assert wait_for(lambda: [u for u, _ in auth.live()] == ["https://p/v1/sensors?kinds=orientation,light"])
    hub.want("level", ["orientation"])  # the same again changes nothing
    time.sleep(0.05)
    assert len(auth.opened) == 2
    hub.want("meter", [])
    assert wait_for(lambda: [u for u, _ in auth.live()] == ["https://p/v1/sensors?kinds=orientation"])
    hub.want("level", ())
    assert wait_for(lambda: auth.live() == [])
    assert hub.status().state == "off" and hub.wanted() == frozenset()


def test_readings_come_in_after_hello_and_only_for_that_stream(hub):
    auth = FakeAuth()
    hub.follow("phone", "https://p/v1", auth, state={"sensors": ALL})
    hub.want("level", ["orientation", "light"])
    assert wait_for(lambda: len(auth.live()) == 1)
    resp = auth.live()[0][1]
    resp.send({"version": 1, "sensors": ["orientation", "light"]})
    assert wait_for(lambda: hub.status().state == "on")
    assert hub.status().streaming == ("orientation", "light")
    resp.send({"t": 1, "orientation": {"roll": 3.5, "pitch": -1.0, "camera": "0"}, "light": 120.0})
    assert wait_for(lambda: hub.reading() is not None)
    resp.send({"t": 2, "orientation": {"roll": None, "pitch": 85.0, "camera": "0"}})
    assert wait_for(lambda: hub.reading().pitch == 85.0)
    reading = hub.reading("phone")
    assert reading.roll is None and reading.light == 120.0 and reading.camera == "0"
    assert hub.reading("other") is None and hub.status("other").state == "off"
    assert "phone" in hub.changes


def test_an_old_phone_is_unavailable_with_a_reason_and_never_asked(hub):
    auth = FakeAuth()
    hub.want("level", ["orientation"])
    hub.follow("old", "https://p/v1", auth, state={"cameras": []})
    status = hub.status()
    assert (status.state, status.reason) == ("unavailable", "old_phone")
    time.sleep(0.05)
    assert auth.opened == []


def test_a_phone_answering_404_is_taken_for_an_old_one(hub):
    hub.want("level", ["orientation"])
    hub.follow("phone", "https://p/v1", FakeAuth(refuse=404), state={"sensors": ALL})
    assert wait_for(lambda: hub.status().state == "unavailable")
    assert hub.status().reason == "old_phone"


def test_a_phone_without_the_sensor_says_so(hub):
    auth = FakeAuth()
    hub.want("meter", ["light"])
    hub.follow("phone", "https://p/v1", auth, state={"sensors": ["orientation"]})
    assert (hub.status().state, hub.status().reason) == ("unavailable", "none")
    assert auth.opened == []
    hub.follow("phone", "https://p/v1", FakeAuth(refuse=409), state={"sensors": ["light"]})
    assert wait_for(lambda: hub.status().state == "unavailable")
    assert hub.status().reason == "none"


def test_something_other_than_a_phone_has_no_sensors(hub):
    hub.want("level", ["orientation"])
    hub.follow("browser", None, None, phone=False)
    assert (hub.status().state, hub.status().reason) == ("unavailable", "not_phone")


def test_it_waits_for_the_phones_state_before_asking(hub):
    auth = FakeAuth()
    hub.want("level", ["orientation"])
    hub.follow("phone", "https://p/v1", auth)
    assert hub.status().state == "starting"
    time.sleep(0.05)
    assert auth.opened == []
    hub.follow("phone", "https://p/v1", auth, state={"sensors": ALL})
    assert wait_for(lambda: len(auth.live()) == 1)


def test_moving_to_another_phone_stops_the_first_and_streams_from_the_second(hub):
    first, second = FakeAuth(), FakeAuth()
    hub.want("level", ["orientation"])
    hub.follow("a", "https://a/v1", first, state={"sensors": ALL})
    assert wait_for(lambda: len(first.live()) == 1)
    first.live()[0][1].send({"t": 1, "orientation": {"roll": 1.0, "pitch": 0.0}})
    assert wait_for(lambda: hub.reading("a") is not None)
    hub.changes.clear()
    hub.follow("b", "https://b/v1", second)
    assert wait_for(lambda: first.live() == [])
    assert "a" in hub.changes  # its readings went
    assert hub.reading("a") is None and hub.reading() is None
    hub.follow("b", "https://b/v1", second, state={"sensors": ALL})
    assert wait_for(lambda: [u for u, _ in second.live()] == ["https://b/v1/sensors?kinds=orientation"])


def test_a_new_route_opens_the_stream_there(hub):
    auth = FakeAuth()
    hub.want("level", ["orientation"])
    hub.follow("phone", "https://usb/v1", auth, state={"sensors": ALL})
    assert wait_for(lambda: len(auth.live()) == 1)
    hub.follow("phone", "https://wifi/v1", auth)  # state still known
    assert wait_for(lambda: [u for u, _ in auth.live()] == ["https://wifi/v1/sensors?kinds=orientation"])


def test_the_stream_stopping_turns_it_off(hub):
    auth = FakeAuth()
    hub.want("level", ["orientation"])
    hub.follow("phone", "https://p/v1", auth, state={"sensors": ALL})
    assert wait_for(lambda: len(auth.live()) == 1)
    hub.follow(None)
    assert wait_for(lambda: auth.live() == [])
    assert hub.status().state == "off" and hub.reading() is None


def test_a_dropped_stream_is_opened_again(hub, monkeypatch):
    monkeypatch.setattr("telescope.sensors._RETRY_S", 0.05)
    auth = FakeAuth()
    hub.want("level", ["orientation"])
    hub.follow("phone", "https://p/v1", auth, state={"sensors": ALL})
    assert wait_for(lambda: len(auth.live()) == 1)
    resp = auth.live()[0][1]
    resp.send({"version": 1, "sensors": ["orientation"]})
    assert wait_for(lambda: hub.status().state == "on")
    resp.close()  # the phone went away
    assert wait_for(lambda: len(auth.opened) == 2 and len(auth.live()) == 1)
    assert hub.status().state == "starting"


def test_diagnostics_only_while_wanted(hub):
    assert hub.diagnostics() == {}
    hub.want("level", ["orientation"])
    assert "orientation" in hub.diagnostics()["Phone sensors"]


def test_unknown_kinds_are_ignored(hub):
    hub.want("x", ["sonar"])
    assert hub.wanted() == frozenset()


def test_a_line_only_changes_what_it_carries():
    reading = apply_line(SensorReading(), {"t": 1, "gravity": [0.1, 9.8, 0.2], "light": 30}, 5.0)
    assert reading.gravity == (0.1, 9.8, 0.2) and reading.light == 30.0 and reading.at == 5.0
    reading = apply_line(reading, {"t": 2, "orientation": {"roll": -4, "pitch": 2.5, "camera": "1"}}, 6.0)
    assert (reading.roll, reading.pitch, reading.camera, reading.light) == (-4.0, 2.5, "1", 30.0)
    assert apply_line(reading, {"t": 3}, 7.0) is reading  # a heartbeat
    assert apply_line(reading, {"orientation": {"roll": "x", "pitch": True}}, 8.0).roll is None
