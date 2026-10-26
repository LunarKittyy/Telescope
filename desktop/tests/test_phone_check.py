"""The device-check tool must measure the outage and exercise automatic handover."""

import importlib.util
from pathlib import Path
from types import SimpleNamespace

import pytest

spec = importlib.util.spec_from_file_location("phone_check", Path(__file__).parents[1] / "scripts" / "phone_check.py")
phone_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(phone_check)


@pytest.mark.parametrize("frames, end, expected", [
    ([9.99, 20.0, 20.033], 20.04, 10.01),
    ([9.99], 20.0, 10.01),
    ([], 20.0, 10.0),
])
def test_gap_measurement_includes_the_last_frame_before_the_operation(monkeypatch, frames, end, expected):
    check = object.__new__(phone_check.Check)
    check.frames = frames
    monkeypatch.setattr(phone_check.time, "monotonic", lambda: end)
    assert check.longest_gap(10.0) == pytest.approx(expected)


def test_handover_uses_automatic_discovery_and_keeps_the_worker():
    from telescope.phones import ROUTE_AUTO

    slots = []
    worker = SimpleNamespace(url="https://phone/v1/video.h264", moved=SimpleNamespace(connect=slots.append))
    check = object.__new__(phone_check.Check)
    check.worker, check.frames = worker, []
    resolver = SimpleNamespace(_adb_device_states=lambda: [("serial", "device")])
    preferences = []
    check.use_route = preferences.append
    check.start = lambda: 0.1 if resolver._adb_device_states() == [] else 0
    check.stop = lambda: None
    check.pump = lambda seconds, until=None: until() if until else False

    def appeared():
        assert resolver._adb_device_states() == [("serial", "device")]
        worker.url = "https://127.0.0.1:12345/v1/video.h264"
        for slot in slots:
            slot(worker.url)

    check.connection = SimpleNamespace(_resolver=resolver, _watch_usb=appeared)
    assert check.handover()
    assert preferences == [ROUTE_AUTO]


def test_a_thin_link_forwards_its_speed_drops_past_its_buffer_and_adds_its_delay():
    link = phone_check.ThinLink(8, buffer_ms=10.5, delay_ms=5, rnd=lambda: 1.0)  # 1 MB/s: 1000 bytes take 1 ms
    for i in range(15):
        link.push(i, 1000, 0.0)
    assert link.passed == 11 and link.dropped == 4  # 10 ms of it queued behind the first, the rest dropped
    assert link.due(0.0061) == [0] and link.due(0.0069) == []
    assert link.due(1.0) == list(range(1, 11))
    lossy = phone_check.ThinLink(8, loss=0.5, rnd=iter([0.1, 0.9]).__next__)
    lossy.push("lost", 100, 0.0)
    lossy.push("kept", 100, 0.0)
    assert lossy.due(1.0) == ["kept"]


def test_lag_is_the_first_frame_that_clearly_changed():
    import numpy as np

    still, brighter = np.zeros((18, 32)), np.full((18, 32), 50.0)
    thumbs = [(t / 10, still) for t in range(10)] + [(1.0 + t / 10, brighter) for t in range(1, 5)]
    assert phone_check.change_lags(thumbs, [0.95]) == [pytest.approx(0.15)]
    assert phone_check.change_lags(thumbs[:10], [0.55]) == [None]  # nothing changed after it
