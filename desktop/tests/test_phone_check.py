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
